//! Bounded parameter/strategy experiments on the shared multi-market Bar engine.

use super::artifacts::{read_text, write_text};
use super::attach;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_continuous_grid,
    validate_experiment_id, validate_grid,
};
use super::experiment_search::{
    continuous_domain_cardinality, ContinuousSearchState, ExperimentSearchAlgorithm,
    ExperimentSearchObjective, ExperimentSearchPlan, ExperimentSearchTrial, SearchState,
};
use super::experiment_spec_artifact::{serialize_experiment_input, verify_spec_artifact};
use super::experiment_split::{
    experiment_folds, summarize_folds, ExperimentFoldReport, FoldRanges,
};
use super::guard::guard_panics;
use super::run_multi_market_backtest::{
    run_inner_with_window, validate_injected, InjectedMarketStrategy, MultiMarketExecutionWindow,
};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, ExperimentParameterRange,
    ExperimentParameterSpace, MultiMarketBacktestOutcome, MultiMarketBacktestSpec, RunContext,
    RunHandle, StrategyArtifactIdentity,
};
use qx_core::Fnv1a;
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategyKind, Strategy};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

mod artifacts;
mod types;
pub use types::*;

pub const RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES: usize = 256;
pub const MAX_MULTI_MARKET_EXPERIMENT_EXECUTIONS: usize = 4096;

type StrategyFactory<'a> = dyn FnMut(&MultiMarketBacktestSpec, &BTreeMap<String, i128>) -> Result<Box<dyn Strategy>, String>
    + 'a;

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
        if let Some(plan) = &self.split_plan {
            plan.validate().map_err(invalid)?;
        }
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
        let callback_count = validate_callback_parameter_grid(&self.callback_parameter_space)?;
        if callback_count > 0
            && (!self.parameter_space.is_empty() || !self.strategy_kinds.is_empty())
        {
            return Err(invalid(
                "callback_parameter_space 只能与自定义策略工厂使用，不能和内置参数/策略网格混用",
            ));
        }
        if callback_count > 0
            && self
                .search
                .as_ref()
                .is_some_and(|search| !search.continuous_parameter_space.is_empty())
        {
            return Err(invalid(
                "callback_parameter_space 不能与 search.continuous_parameter_space 混用",
            ));
        }
        if self.parameter_space.is_empty() && self.strategy_kinds.is_empty() && callback_count == 0
        {
            if self
                .search
                .as_ref()
                .is_none_or(|search| search.continuous_parameter_space.is_empty())
            {
                return Err(invalid("多市场实验至少需要参数维度或 strategy_kinds"));
            }
        }
        if let Some(search) = &self.search {
            if self.split_plan.is_none() {
                return Err(invalid(
                    "多市场自适应搜索要求配置 split_plan，只能用训练窗提案和选参",
                ));
            }
            if !self.strategy_kinds.is_empty() {
                return Err(invalid(
                    "多市场自适应搜索当前针对单个策略槽的参数，不接受 strategy_kinds",
                ));
            }
            if search.budget == 0 || search.budget > MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES {
                return Err(invalid(format!(
                    "多市场自适应搜索预算必须在 1..={MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES}"
                )));
            }
            if search
                .max_training_drawdown_bps
                .is_some_and(|limit| limit > 10_000)
            {
                return Err(invalid("max_training_drawdown_bps 不得大于 10000"));
            }
            if search.early_pruning.is_some()
                || search.robustness_subwindows.is_some()
                || search.minimum_training_robustness_score_bps.is_some()
                || search.neighborhood_radius.is_some()
            {
                return Err(invalid(
                    "多市场自适应搜索暂不支持 early_pruning、robustness_subwindows、minimum_training_robustness_score_bps 或 neighborhood_radius",
                ));
            }
            let domain = if !search.continuous_parameter_space.is_empty() {
                if !self.parameter_space.is_empty() || callback_count > 0 {
                    return Err(invalid("continuous_parameter_space 不能与离散参数网格混用"));
                }
                if self.search_uses_callback_ranges() {
                    validate_callback_parameter_ranges(&search.continuous_parameter_space)?
                } else {
                    validate_continuous_grid(base_strategy, &search.continuous_parameter_space)?
                }
            } else if callback_count > 0 {
                callback_count as u128
            } else if !self.parameter_space.is_empty() {
                validate_grid(
                    &normalized_strategy(base_strategy, &self.parameter_space),
                    &self.parameter_space,
                )? as u128
            } else {
                return Err(invalid(
                    "多市场自适应搜索至少需要 parameter_space、callback_parameter_space 或 continuous_parameter_space",
                ));
            };
            if search.budget as u128 > domain {
                return Err(invalid(format!(
                    "多市场搜索预算 {} 超过参数值域大小 {domain}",
                    search.budget
                )));
            }
            return Ok(search.budget);
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
        if callback_count > 0 {
            total = total
                .checked_mul(callback_count)
                .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES)
                .ok_or_else(|| invalid("多市场实验最多接受 256 个候选组合"))?;
        }
        Ok(total)
    }

    fn search_uses_callback_ranges(&self) -> bool {
        self.search.as_ref().is_some_and(|search| {
            search
                .continuous_parameter_space
                .iter()
                .any(|range| !is_builtin_search_parameter(&range.name))
        })
    }

    fn supports_parameter_factory(&self) -> bool {
        !self.callback_parameter_space.is_empty()
            || self.search_uses_callback_ranges()
            || (self.search.is_some()
                && (!self.parameter_space.is_empty()
                    || self
                        .search
                        .as_ref()
                        .is_some_and(|search| !search.continuous_parameter_space.is_empty())))
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
        if !self.strategy_kinds.is_empty()
            || !self.callback_parameter_space.is_empty()
            || self.search_uses_callback_ranges()
        {
            return Err(invalid(
                "该自定义策略工厂不接受任意回调参数；请使用 run_with_parameter_factory 和 callback_parameter_space 或自定义连续范围",
            ));
        }
        validate_identity(self, identity)?;
        let mut adapted_factory =
            |candidate: &MultiMarketBacktestSpec, _parameters: &BTreeMap<String, i128>| {
                factory(candidate)
            };
        guard_panics(&correlation_id, || {
            run_inner_experiment(
                self,
                context,
                None,
                Some(identity),
                Some(&mut adapted_factory),
            )
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
        if !self.strategy_kinds.is_empty()
            || !self.callback_parameter_space.is_empty()
            || self.search_uses_callback_ranges()
        {
            return Err(invalid(
                "该自定义策略工厂不接受任意回调参数；请使用 start_with_parameter_factory 和 callback_parameter_space 或自定义连续范围",
            ));
        }
        validate_identity(&self, &identity).map_err(|error| attach(error, &correlation_id))?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            let mut adapted_factory =
                |candidate: &MultiMarketBacktestSpec, _parameters: &BTreeMap<String, i128>| {
                    factory(candidate)
                };
            guard_panics(&correlation_id, || {
                run_inner_experiment(
                    &self,
                    &context,
                    Some(&cancellation),
                    Some(&identity),
                    Some(&mut adapted_factory),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }

    pub fn run_with_parameter_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunMultiMarketExperimentResult, AppError>
    where
        F: FnMut(
            &MultiMarketBacktestSpec,
            &BTreeMap<String, i128>,
        ) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.supports_parameter_factory() || !self.strategy_kinds.is_empty() {
            return Err(attach(
                invalid(
                    "任意参数工厂必须只使用 callback_parameter_space 或 custom continuous ranges",
                ),
                &correlation_id,
            ));
        }
        validate_identity(self, identity).map_err(|error| attach(error, &correlation_id))?;
        guard_panics(&correlation_id, || {
            run_inner_experiment(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| attach(invariant("同步多市场策略实验未产生报告"), &correlation_id))
    }

    pub fn start_with_parameter_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunMultiMarketExperimentResult>, AppError>
    where
        F: FnMut(
                &MultiMarketBacktestSpec,
                &BTreeMap<String, i128>,
            ) -> Result<Box<dyn Strategy>, String>
            + Send
            + 'static,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.supports_parameter_factory() || !self.strategy_kinds.is_empty() {
            return Err(attach(
                invalid(
                    "任意参数工厂必须只使用 callback_parameter_space 或 custom continuous ranges",
                ),
                &correlation_id,
            ));
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
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            let mut result = existing_result(
                &existing,
                &spec_fingerprint,
                &canonical_run_spec_fingerprint,
                &spec.experiment_id,
            )?;
            verify_spec_artifact(&spec_path, &canonical_spec, "多市场实验")?;
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
                if result.canonical_run_spec_json.as_deref() != Some(&canonical_run_spec_json)
                    || result.canonical_run_spec_fingerprint.as_deref()
                        != Some(&canonical_run_spec_fingerprint)
                    || result.input_data_fingerprint.as_deref() != Some(&input_data_fingerprint)
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
                        || outcome.markets_total as usize != canonical_run_spec.datasets.len()
                }) {
                    return Err(AppError::new(
                        AppErrorCategory::Conflict,
                        "既有多市场实验候选绑定的数据与当前输入不一致，不能升级运行身份",
                    ));
                }
            }
            result.canonical_run_spec_json = Some(canonical_run_spec_json.clone());
            result.canonical_run_spec_fingerprint = Some(canonical_run_spec_fingerprint.clone());
            result.input_data_fingerprint = Some(input_data_fingerprint.clone());
            artifacts::write_input_spec(&spec_path, &canonical_spec)?;
            let upgraded = artifacts::persist_result(
                result,
                &canonical_run_spec,
                &root,
                &artifact_path,
                &spec_path,
            )?;
            artifacts::verify_persisted_result(&upgraded)?;
            return Ok(Some(upgraded));
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
    if callback_parameter_search
        && (!spec.parameter_space.is_empty() || !spec.strategy_kinds.is_empty())
    {
        return Err(invalid(
            "callback_parameter_space 不能与内置参数/策略网格混用",
        ));
    }
    let active_parameter_space = if callback_parameter_search {
        &spec.callback_parameter_space
    } else {
        &spec.parameter_space
    };
    let combinations = if active_parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(active_parameter_space)
    };
    let mut candidate_plans = Vec::with_capacity(total);
    if spec.search.is_none() {
        for kind in kinds {
            for parameters in &combinations {
                candidate_plans.push(CandidatePlan {
                    ordinal: candidate_plans.len() + 1,
                    kind,
                    parameters: parameters.clone(),
                    proposal: None,
                    search_index: None,
                });
            }
        }
        if candidate_plans.len() != total {
            return Err(invariant("多市场实验候选计划数量与规格校验数量不一致"));
        }
    }
    let timeline = spec
        .split_plan
        .as_ref()
        .map(|_| load_multi_market_timeline(&spec.base))
        .transpose()?;
    let folds = match (&spec.split_plan, timeline.as_deref()) {
        (Some(plan), Some(timeline)) => experiment_folds(plan, timeline.len())?,
        (None, None) => Vec::new(),
        _ => return Err(invariant("多市场实验切分计划与时间轴状态不一致")),
    };
    if spec.split_plan.is_some() {
        let executions = folds
            .len()
            .checked_mul(total.saturating_add(1))
            .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_EXECUTIONS)
            .ok_or_else(|| {
                invalid(format!(
                    "多市场实验训练/样本外执行总数超过上限 {MAX_MULTI_MARKET_EXPERIMENT_EXECUTIONS}"
                ))
            })?;
        if executions == 0 {
            return Err(invalid("多市场实验切分未生成训练与样本外执行"));
        }
    }
    let first_training_window = match (timeline.as_deref(), folds.first()) {
        (Some(timeline), Some(fold)) => Some(window_for_range(timeline, fold.train)?),
        (None, None) => None,
        _ => return Err(invariant("多市场实验首折与时间轴状态不一致")),
    };
    let mut make_injected = |candidate: &MultiMarketBacktestSpec,
                             parameters: &BTreeMap<String, i128>|
     -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError> {
        match (identity, factory.as_deref_mut()) {
            (Some(identity), Some(factory)) => {
                let strategy = factory(candidate, parameters).map_err(invalid)?;
                // The injected strategy replaces this slot. Bind its identity to the original
                // built-in contract instead of the candidate's varied parameter values.
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
                validate_injected(&binding_spec, vec![(identity.clone(), strategy)])
            }
            (None, None) => Ok(BTreeMap::new()),
            _ => Err(invariant("多市场实验策略工厂和身份必须同时提供")),
        }
    };
    let mut candidates = Vec::with_capacity(total);
    let mut first_search = spec.search.clone().map(|search| {
        MultiMarketSearchGenerator::new(
            search,
            spec.search.as_ref().map_or(0, |plan| plan.seed),
            active_parameter_space.clone(),
            combinations.clone(),
        )
    });
    for ordinal in 1..=total {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let plan = match first_search.as_mut() {
            Some(search) => search.next(ordinal, target_kind)?,
            None => candidate_plans
                .get(ordinal - 1)
                .cloned()
                .ok_or_else(|| invariant("多市场实验候选计划数量不足"))?,
        };
        if first_search.is_some() {
            candidate_plans.push(plan.clone());
        }
        let candidate = run_training_candidate(
            spec,
            &plan,
            &root,
            folds.first().map(|_| 1),
            "train",
            callback_parameter_search,
            first_training_window,
            context,
            cancellation,
            &mut make_injected,
        )?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        if let Some(search) = first_search.as_mut() {
            search.observe(&plan, Some(&candidate));
        }
        candidates.push(candidate);
    }
    let mut fold_searches = Vec::new();
    if let (Some(search), Some(fold), Some(generator)) =
        (spec.search.as_ref(), folds.first(), first_search.take())
    {
        fold_searches.push(MultiMarketExperimentFoldSearchReport {
            fold: 1,
            seed: generator.seed,
            train_range: range_to_u64(fold.train)?,
            test_range: range_to_u64(fold.test)?,
            trials: generator.trials,
        });
        debug_assert_eq!(search.budget, total);
    }
    let ranking = rank_candidates(&candidates);
    let mut out_of_sample = None;
    let mut fold_candidate_failures = candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .error
                .as_ref()
                .map(|error| MultiMarketExperimentFoldCandidateFailure {
                    fold: 1,
                    ordinal: candidate.ordinal,
                    error: error.clone(),
                })
        })
        .collect::<Vec<_>>();
    if let Some(timeline) = timeline.as_deref() {
        if ranking.is_empty() {
            let first_failure = candidates
                .iter()
                .find_map(|candidate| candidate.error.as_deref())
                .unwrap_or("无候选错误详情");
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "多市场实验首个训练窗没有成功候选，无法生成样本外验证；首个候选失败原因：{first_failure}"
                ),
            ));
        }
        let mut fold_reports = Vec::with_capacity(folds.len());
        for (fold_index, fold) in folds.iter().enumerate() {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let mut fold_plans = if fold_index == 0 {
                candidate_plans.clone()
            } else {
                Vec::new()
            };
            let mut fold_training = Vec::<(usize, MultiMarketBacktestOutcome)>::new();
            if fold_index == 0 {
                fold_training.extend(candidates.iter().filter_map(|candidate| {
                    candidate.outcome.as_ref().and_then(|outcome| {
                        spec.search
                            .as_ref()
                            .and_then(|plan| plan.max_training_drawdown_bps)
                            .is_none_or(|limit| outcome.max_drawdown_bps <= limit)
                            .then(|| (candidate.ordinal, outcome.clone()))
                    })
                }));
            } else {
                let window = window_for_range(timeline, fold.train)?;
                let fold_local_search = spec
                    .search
                    .as_ref()
                    .is_some_and(|search| search.fold_local_search);
                let mut generator = if fold_local_search {
                    spec.search.clone().map(|mut search| {
                        let seed = search_seed(search.seed, fold_index);
                        search.seed = seed;
                        MultiMarketSearchGenerator::new(
                            search,
                            seed,
                            active_parameter_space.clone(),
                            combinations.clone(),
                        )
                    })
                } else {
                    None
                };
                for ordinal in 1..=if generator.is_some() {
                    total
                } else {
                    candidate_plans.len()
                } {
                    if cancellation.is_some_and(CancellationToken::is_cancelled) {
                        return Ok(None);
                    }
                    let plan = match generator.as_mut() {
                        Some(search) => search.next(ordinal, target_kind)?,
                        None => candidate_plans
                            .get(ordinal - 1)
                            .cloned()
                            .ok_or_else(|| invariant("多市场折内候选计划数量不足"))?,
                    };
                    if generator.is_some() {
                        fold_plans.push(plan.clone());
                    }
                    let candidate = run_training_candidate(
                        spec,
                        &plan,
                        &root,
                        Some(fold_index + 1),
                        "train",
                        callback_parameter_search,
                        Some(window),
                        context,
                        cancellation,
                        &mut make_injected,
                    )?;
                    let Some(candidate) = candidate else {
                        return Ok(None);
                    };
                    if let Some(search) = generator.as_mut() {
                        search.observe(&plan, Some(&candidate));
                    }
                    if let Some(outcome) = &candidate.outcome {
                        if spec
                            .search
                            .as_ref()
                            .and_then(|search| search.max_training_drawdown_bps)
                            .is_none_or(|limit| outcome.max_drawdown_bps <= limit)
                        {
                            fold_training.push((candidate.ordinal, outcome.clone()));
                        }
                    }
                    if let Some(error) = candidate.error {
                        fold_candidate_failures.push(MultiMarketExperimentFoldCandidateFailure {
                            fold: fold_index + 1,
                            ordinal: plan.ordinal,
                            error,
                        });
                    }
                }
                if let (Some(generator), Some(search)) = (generator, spec.search.as_ref()) {
                    fold_searches.push(MultiMarketExperimentFoldSearchReport {
                        fold: fold_index + 1,
                        seed: generator.seed,
                        train_range: range_to_u64(fold.train)?,
                        test_range: range_to_u64(fold.test)?,
                        trials: generator.trials,
                    });
                    debug_assert!(search.fold_local_search);
                }
            }
            fold_training.sort_by(|left, right| {
                right
                    .1
                    .return_bps
                    .cmp(&left.1.return_bps)
                    .then_with(|| left.1.max_drawdown_bps.cmp(&right.1.max_drawdown_bps))
                    .then_with(|| left.0.cmp(&right.0))
            });
            let (selected_ordinal, selected_training) =
                fold_training.into_iter().next().ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::FidelityInsufficient,
                        format!("多市场实验第 {} 折训练窗没有成功候选", fold_index + 1),
                    )
                })?;
            let selected_plan = if fold_index > 0
                && spec
                    .search
                    .as_ref()
                    .is_some_and(|search| search.fold_local_search)
            {
                fold_plans
                    .iter()
                    .find(|plan| plan.ordinal == selected_ordinal)
            } else {
                candidate_plans
                    .iter()
                    .find(|plan| plan.ordinal == selected_ordinal)
            }
            .ok_or_else(|| invariant("多市场实验选中的候选计划不存在"))?;
            let (test_candidate, candidate_error) = build_candidate_spec(
                spec,
                selected_plan,
                &root,
                Some(fold_index + 1),
                "oos",
                callback_parameter_search,
            )?;
            if let Some(error) = candidate_error {
                return Err(error);
            }
            let injected = make_injected(&test_candidate, &selected_plan.parameters)?;
            let test_window = window_for_range(timeline, fold.test)?;
            let test_outcome = run_inner_with_window(
                &test_candidate,
                context,
                cancellation,
                injected,
                Some(test_window),
                Some(&selected_plan.parameters),
            )?
            .ok_or_else(|| invariant("多市场样本外执行意外取消"))?;
            fold_reports.push(to_experiment_fold_report(
                fold_index + 1,
                *fold,
                selected_ordinal,
                selected_plan,
                !spec.strategy_kinds.is_empty(),
                spec.search
                    .as_ref()
                    .and_then(|search| search.max_training_drawdown_bps),
                &selected_training,
                &test_outcome,
            )?);
        }
        out_of_sample = Some(summarize_folds(fold_reports));
    }
    let succeeded_candidates = ranking.len();
    let search_report = spec
        .search
        .as_ref()
        .map(|search| MultiMarketExperimentSearchReport {
            algorithm: search.algorithm,
            objective: if search.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                ExperimentSearchObjective::ParetoReturnDrawdown
            } else {
                ExperimentSearchObjective::Return
            },
            seed: search.seed,
            budget: search.budget,
            max_training_drawdown_bps: search.max_training_drawdown_bps,
            fold_local_search: search.fold_local_search,
            fold_searches,
        });
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
        out_of_sample,
        fold_candidate_failures,
        search: search_report,
        artifact_path: artifact_path.clone(),
        canonical_run_spec_json: Some(canonical_run_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_run_spec_fingerprint),
        input_data_fingerprint: Some(input_data_fingerprint),
        result_hash: None,
        run_manifest: None,
        run_record: None,
        artifact_locations: None,
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建多市场实验目录", &error))?;
    artifacts::write_input_spec(&spec_path, &canonical_spec)?;
    let persisted = artifacts::persist_result(
        result,
        &canonical_run_spec,
        &root,
        &artifact_path,
        &spec_path,
    )?;
    artifacts::verify_persisted_result(&persisted)?;
    Ok(Some(persisted))
}

#[derive(Clone)]
struct CandidatePlan {
    ordinal: usize,
    kind: BuiltinStrategyKind,
    parameters: BTreeMap<String, i128>,
    proposal: Option<String>,
    search_index: Option<usize>,
}

struct MultiMarketSearchGenerator {
    plan: ExperimentSearchPlan,
    seed: u64,
    dimensions: Vec<ExperimentParameterSpace>,
    combinations: Vec<BTreeMap<String, i128>>,
    ranges: Vec<ExperimentParameterRange>,
    discrete: Option<SearchState>,
    continuous: Option<ContinuousSearchState>,
    trials: Vec<ExperimentSearchTrial>,
    observed_scores: Vec<(usize, i32)>,
    observed_drawdowns: Vec<(usize, u32)>,
}

impl MultiMarketSearchGenerator {
    fn new(
        plan: ExperimentSearchPlan,
        seed: u64,
        dimensions: Vec<ExperimentParameterSpace>,
        combinations: Vec<BTreeMap<String, i128>>,
    ) -> Self {
        let ranges = plan.continuous_parameter_space.clone();
        let continuous = (!ranges.is_empty()).then(|| ContinuousSearchState::new(&ranges, seed));
        let discrete = ranges
            .is_empty()
            .then(|| SearchState::new(combinations.len(), seed));
        Self {
            plan,
            seed,
            dimensions,
            combinations,
            ranges,
            discrete,
            continuous,
            trials: Vec::new(),
            observed_scores: Vec::new(),
            observed_drawdowns: Vec::new(),
        }
    }

    fn next(
        &mut self,
        ordinal: usize,
        kind: BuiltinStrategyKind,
    ) -> Result<CandidatePlan, AppError> {
        let objective = if self.plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
            ExperimentSearchObjective::ParetoReturnDrawdown
        } else {
            ExperimentSearchObjective::Return
        };
        let (parameters, proposal, search_index) = if self.ranges.is_empty() {
            let eligible_scores = self
                .observed_scores
                .iter()
                .copied()
                .filter(|(index, _)| {
                    self.plan.max_training_drawdown_bps.is_none_or(|limit| {
                        self.observed_drawdowns
                            .iter()
                            .any(|(drawdown_index, drawdown)| {
                                drawdown_index == index && *drawdown <= limit
                            })
                    })
                })
                .collect::<Vec<_>>();
            let eligible_drawdowns = self
                .observed_drawdowns
                .iter()
                .copied()
                .filter(|(_, drawdown)| {
                    self.plan
                        .max_training_drawdown_bps
                        .is_none_or(|limit| *drawdown <= limit)
                })
                .collect::<Vec<_>>();
            let (index, proposal) = self
                .discrete
                .as_mut()
                .and_then(|state| {
                    state.next_with_objective(
                        self.plan.algorithm,
                        objective,
                        4,
                        &self.dimensions,
                        &self.combinations,
                        &eligible_scores,
                        &eligible_drawdowns,
                    )
                })
                .ok_or_else(|| invariant("多市场自适应搜索提前耗尽离散候选"))?;
            (
                self.combinations[index].clone(),
                proposal.to_string(),
                Some(index),
            )
        } else {
            let eligible_trials = self
                .trials
                .iter()
                .filter(|trial| {
                    self.plan.max_training_drawdown_bps.is_none_or(|limit| {
                        trial
                            .training_max_drawdown_bps
                            .is_some_and(|drawdown| drawdown <= limit)
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            let (parameters, proposal) = self
                .continuous
                .as_mut()
                .and_then(|state| {
                    state.next(
                        self.plan.algorithm,
                        objective,
                        4,
                        &self.ranges,
                        &eligible_trials,
                    )
                })
                .ok_or_else(|| invariant("多市场自适应搜索提前耗尽连续候选"))?;
            (parameters, proposal.to_string(), None)
        };
        Ok(CandidatePlan {
            ordinal,
            kind,
            parameters,
            proposal: Some(proposal),
            search_index,
        })
    }

    fn observe(
        &mut self,
        candidate: &CandidatePlan,
        outcome: Option<&MultiMarketExperimentCandidate>,
    ) {
        let training_score_bps = outcome
            .and_then(|candidate| candidate.outcome.as_ref())
            .map(|run| run.return_bps);
        let training_max_drawdown_bps = outcome
            .and_then(|candidate| candidate.outcome.as_ref())
            .map(|run| run.max_drawdown_bps);
        self.trials.push(ExperimentSearchTrial {
            ordinal: candidate.ordinal,
            proposal: candidate
                .proposal
                .clone()
                .unwrap_or_else(|| "random".into()),
            parameters: candidate.parameters.clone(),
            training_score_bps,
            training_max_drawdown_bps,
            training_robustness_score_bps: None,
        });
        if let (Some(index), Some(score), Some(drawdown)) = (
            candidate.search_index,
            training_score_bps,
            training_max_drawdown_bps,
        ) {
            self.observed_scores.push((index, score));
            self.observed_drawdowns.push((index, drawdown));
        }
    }
}

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

fn build_candidate_spec(
    experiment: &RunMultiMarketExperimentSpec,
    plan: &CandidatePlan,
    root: &std::path::Path,
    fold: Option<usize>,
    phase: &str,
    callback_parameter_search: bool,
) -> Result<(MultiMarketBacktestSpec, Option<AppError>), AppError> {
    let mut candidate = experiment.base.clone();
    candidate.run_id = match fold {
        Some(fold) => format!(
            "{}-f{fold:04}-{phase}-{:04}",
            experiment.experiment_id, plan.ordinal
        ),
        None => format!("{}-{:04}", experiment.experiment_id, plan.ordinal),
    };
    candidate.output_dir = match fold {
        Some(fold) => root
            .join(format!("fold-{fold:04}"))
            .join(format!("{phase}-{:04}", plan.ordinal))
            .to_string_lossy()
            .into_owned(),
        None => root
            .join(format!("run-{:04}", plan.ordinal))
            .to_string_lossy()
            .into_owned(),
    };
    let target = candidate
        .markets
        .iter_mut()
        .flat_map(|market| &mut market.strategies)
        .find(|slot| slot.strategy.strategy_id == experiment.target_strategy_id)
        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
    target.strategy.kind = plan.kind;
    if callback_parameter_search {
        return Ok((candidate, None));
    }
    let error = apply_parameters(&mut target.strategy, &plan.parameters).err();
    Ok((candidate, error))
}

fn rank_candidates(
    candidates: &[MultiMarketExperimentCandidate],
) -> Vec<MultiMarketExperimentRankedCandidate> {
    let mut ranked = candidates
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
    ranked.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked
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
        .collect()
}

fn load_multi_market_timeline(spec: &MultiMarketBacktestSpec) -> Result<Vec<u64>, AppError> {
    let mut timeline = BTreeSet::new();
    for market in &spec.markets {
        let payload = read_text(&market.bars_path)?;
        let frame = BarFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} BarFrame 无效，无法生成实验时间切分: {error:?}",
                market.instrument
            ))
        })?;
        if frame.instrument.to_string() != market.instrument {
            return Err(invalid(format!(
                "市场 {} 的 BarFrame 实际标的是 {}",
                market.instrument, frame.instrument
            )));
        }
        timeline.extend(frame.ts.iter().copied());
    }
    if timeline.len() < crate::MIN_BACKTEST_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "多市场实验联合时间轴至少需要两根 Bar",
        ));
    }
    Ok(timeline.into_iter().collect())
}

fn window_for_range(
    timeline: &[u64],
    range: [usize; 2],
) -> Result<MultiMarketExecutionWindow, AppError> {
    if range[0] >= range[1] || range[1] > timeline.len() {
        return Err(invariant("多市场实验窗口超出联合时间轴范围"));
    }
    Ok(MultiMarketExecutionWindow {
        trade_start_ts: Some(timeline[range[0]]),
        data_end_ts_exclusive: (range[1] < timeline.len()).then(|| timeline[range[1]]),
    })
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

fn to_experiment_fold_report(
    fold_number: usize,
    ranges: FoldRanges,
    selected_ordinal: usize,
    selected_plan: &CandidatePlan,
    selected_strategy_kind: bool,
    max_training_drawdown_bps: Option<u32>,
    training: &crate::MultiMarketBacktestOutcome,
    test: &crate::MultiMarketBacktestOutcome,
) -> Result<ExperimentFoldReport, AppError> {
    let range = |value: [usize; 2]| -> Result<[u64; 2], AppError> {
        Ok([
            u64::try_from(value[0]).map_err(|_| invalid("实验折索引超过 u64 范围"))?,
            u64::try_from(value[1]).map_err(|_| invalid("实验折索引超过 u64 范围"))?,
        ])
    };
    Ok(ExperimentFoldReport {
        fold: fold_number,
        train_range: range(ranges.train)?,
        test_range: range(ranges.test)?,
        purge_range: (ranges.purge[0] < ranges.purge[1])
            .then(|| range(ranges.purge))
            .transpose()?,
        embargo_range: (ranges.embargo[0] < ranges.embargo[1])
            .then(|| range(ranges.embargo))
            .transpose()?,
        selected_ordinal,
        selected_strategy_kind: selected_strategy_kind.then(|| selected_plan.kind.name().into()),
        selected_parameters: selected_plan.parameters.clone(),
        in_sample_return_bps: training.return_bps,
        selected_training_robustness_score_bps: None,
        selected_training_max_drawdown_bps: Some(training.max_drawdown_bps),
        max_training_drawdown_bps,
        out_of_sample_return_bps: test.return_bps,
    })
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
