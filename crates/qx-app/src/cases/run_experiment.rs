//! Deterministic Cartesian parameter experiments over the shared Rust backtest use case.
//!
//! This module only expands and records strategy parameters. Every candidate is executed by
//! [`crate::cases::run_backtest::run_backtest`], and successful candidates are ranked by the
//! shared [`crate::cases::compare_runs::compare_runs`] use case; it contains no matcher,
//! risk, ledger, or metrics implementation.
//!
//! 两个**可选**旋钮把方案 §8「参数实验」点名的两件事真正做出来（V13 R28）：
//! `split_plan` 走训练/测试切分与滚动走查、`baselines` 出 buy-and-hold / zero-signal 对照列。
//! 两者都不写时这条路径与加它们之前**逐字节相同**（`skip_serializing_if`），所以旧
//! `.experiment.json` 的规格指纹不会漂移。切分与基准的算法在 [`super::experiment_split`]，
//! 本模块只负责在读侧把它们接进来。

use super::artifacts::{equity_series, persist_run_manifest, read_text, write_text};
use super::compare_runs::COMPARE_RUNS_SCHEMA_VERSION;
use super::experiment_fold_search::{
    evaluate_fold_local_search, fold_local_execution_count, load_cost_scenario_checkpoint,
    search_execution_count, write_cost_scenario_checkpoint, MAX_FOLD_LOCAL_RUNS,
};
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_continuous_grid,
    validate_experiment_id, validate_grid,
};
use super::experiment_search::{
    continuous_domain_cardinality, continuous_neighborhood_report_with_radius_and_budget,
    neighborhood_parameter_sets, neighborhood_report_with_radius_and_budget, ContinuousSearchState,
    DrawdownPruningWindow, ExperimentCandidatePruning, ExperimentSearchAlgorithm,
    ExperimentSearchObjective, ExperimentSearchPlan, ExperimentSearchReport, ExperimentSearchTrial,
    SearchBacktestAttempt, SearchState,
};
use super::experiment_spec_artifact::{serialize_experiment_input, write_spec_artifact};
pub use super::experiment_split::EXPERIMENT_BASELINE_NAMES;
use super::experiment_split::{
    evaluate_baselines, evaluate_out_of_sample, experiment_folds, training_metrics_for_equity,
    window_max_drawdown_bps, window_return_bps, window_robustness_score_bps,
    ExperimentBaselineReport, ExperimentOutOfSampleReport, ExperimentSplitPlan,
};
use super::guard::guard_panics;
use super::run_backtest::{
    run_backtest_inner, run_backtest_inner_for_experiment,
    run_backtest_inner_with_experiment_strategy,
};
use super::verify_run::verify_run;
use super::{attach, compare_runs};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, BacktestOutcome, BacktestSpec, CallerCapability, CompareRunsResult,
    CompareRunsSpec, ComparedRun, RunContext, RunHandle, StrategyArtifactIdentity,
};
use qx_core::{Fnv1a, RunManifest};
use qx_datastruct::BarFrame;
use qx_spec::{ArtifactRefV2, CanonicalRunSpec, ReplayVerdict, RunRecordV2, RunStatus};
use qx_strategy::BuiltinStrategyKind;
use qx_strategy::Strategy;
use qx_xingban::{AshareRuleConfig, ExecutionCostRules};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod candidate_execution;
mod cost_sensitivity;
mod identity;
mod persistence;
mod recovery;
mod report_assembly;
pub(super) use cost_sensitivity::{cost_robustness_passed, cost_sensitivity_fold_delta};
use identity::{
    build_experiment_canonical_run_spec, experiment_result_hash,
    validate_experiment_artifact_envelope, validate_experiment_run_identity,
};
pub(crate) use identity::{
    experiment_result_hash_from_json, experiment_result_hash_from_value,
    normalize_experiment_identity_values, quote_unsafe_integer_tokens,
};
pub(crate) use persistence::build_experiment_run_manifest;
use report_assembly::{assemble_and_persist, ExperimentResultAssemblyInput};

pub const RUN_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_EXPERIMENT_CANDIDATES: usize = 256;

pub(super) type StrategyFactory<'a> =
    dyn FnMut(&BacktestSpec) -> Result<Box<dyn Strategy>, String> + 'a;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentParameterSpace {
    /// One of the signal, fixed-point order-size, or mutually exclusive sizing parameters supported by `validate_grid`.
    pub name: String,
    pub values: Vec<i128>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub base: BacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional strategy-selection dimension for a same-dataset comparison. When empty, the
    /// legacy single-strategy experiment shape and fingerprint remain unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
    /// 可选的数据切分计划。`None` = 不切分。
    ///
    /// `skip_serializing_if` 是**刻意的**：`spec_fingerprint` 是 `to_json()` 的哈希，
    /// 而 `existing_result` 用它判定"这份产物是不是同一份实验"。新加两格若照常序列化，
    /// 任何在本构建之前写下的 `.experiment.json` 都会因为指纹变了而报 `Conflict`——
    /// 那是把一次**纯加法**升级变成了旧产物的拒绝服务（V13 R27 `JobSpec` 同一类教训）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_plan: Option<ExperimentSplitPlan>,
    /// 基准对照清单，取值域 [`EXPERIMENT_BASELINE_NAMES`]。空表 = 不算基准。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baselines: Vec<String>,
    /// Deterministic adaptive search; requires a train/test split and is fingerprinted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<ExperimentSearchPlan>,
    /// Optional execution-cost stress scenarios. Each scenario reruns the training-selected
    /// candidate through the normal matcher, fee, ledger and replay path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cost_sensitivity: Vec<ExecutionCostSensitivityScenario>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCostSensitivityScenario {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_cost_rules: Option<ExecutionCostRules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ashare_rules: Option<AshareRuleConfig>,
    pub fill_model: crate::BacktestFillModel,
    /// Optional maximum training-return loss from baseline under this cost scenario, in bps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_training_return_degradation_bps: Option<u32>,
    /// Require every neighbor in the configured training neighborhood to pass its own stress replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_neighborhood_training_return_degradation_bps: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCostSensitivityNeighborResult {
    pub ordinal: usize,
    pub parameters: BTreeMap<String, i128>,
    pub fold_deltas: Vec<ExecutionCostSensitivityFoldDelta>,
    pub cost_robustness_passed: bool,
    pub outcome: Option<BacktestOutcome>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCostSensitivityNeighborhoodResult {
    pub radius: usize,
    pub possible_neighbors: usize,
    pub evaluated_neighbors: usize,
    pub successful_neighbors: usize,
    pub passed_neighbors: usize,
    pub complete: bool,
    pub passed: bool,
    pub candidates: Vec<ExecutionCostSensitivityNeighborResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCostSensitivityResult {
    pub selected_ordinal: usize,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_cost_rules: Option<ExecutionCostRules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ashare_rules: Option<AshareRuleConfig>,
    pub fill_model: crate::BacktestFillModel,
    /// Whole replay interval delta retained for v1 consumers; split-aware consumers should use `fold_deltas`.
    pub return_delta_bps: Option<i64>,
    /// Whole replay interval delta retained for v1 consumers; split-aware consumers should use `fold_deltas`.
    pub drawdown_delta_bps: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_training_return_degradation_bps: Option<u32>,
    /// False when a configured gate is breached or its stress result is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_robustness_passed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_neighborhood_training_return_degradation_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighborhood_cost_robustness: Option<ExecutionCostSensitivityNeighborhoodResult>,
    /// Per-fold deltas keep in-sample cost sensitivity separate from OOS evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fold_deltas: Vec<ExecutionCostSensitivityFoldDelta>,
    pub outcome: Option<BacktestOutcome>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCostSensitivityFoldDelta {
    pub fold: usize,
    pub train_range: [u64; 2],
    pub test_range: [u64; 2],
    pub training_return_delta_bps: i64,
    pub training_drawdown_delta_bps: i64,
    pub out_of_sample_return_delta_bps: i64,
    pub out_of_sample_drawdown_delta_bps: i64,
}

impl RunExperimentSpec {
    /// Run a bounded parameter experiment with a fresh caller-authored strategy per candidate.
    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunExperimentResult, AppError>
    where
        F: FnMut(&BacktestSpec) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context.require(CallerCapability::Research, "多语言策略参数回测实验")?;
        validate_strategy_experiment(self, identity)?;
        guard_panics(&correlation_id, || {
            run_experiment_inner(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步多语言策略参数实验未产生报告",
                ),
                &correlation_id,
            )
        })
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("RunExperimentSpec JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunExperimentSpec 序列化失败: {error}"),
            )
        })
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_EXPERIMENT_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "RunExperimentSpec schema_version={} 本版本只认 {}",
                    self.schema_version, RUN_EXPERIMENT_SCHEMA_VERSION
                ),
            ));
        }
        validate_experiment_id(&self.experiment_id)?;
        // Validate all candidate kinds against the shared parameter dimensions before any run
        // starts. Two-leg kinds belong to RunMultiLegExperimentSpec and must not be silently
        // routed through the single-frame Bar engine.
        let strategy_selection = !self.strategy_kinds.is_empty();
        let mut kinds = if strategy_selection {
            self.strategy_kinds.clone()
        } else {
            vec![self.base.strategy.kind]
        };
        kinds.sort_by_key(|kind| kind.name());
        let mut seen_kinds = BTreeSet::new();
        if kinds.iter().any(|kind| !seen_kinds.insert(kind.name())) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "strategy_kinds 不得重复",
            ));
        }
        if kinds.iter().any(|kind| kind.needs_reference_leg()) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "双腿策略必须使用 RunMultiLegExperimentSpec 和双数据集",
            ));
        }
        // The strategy-only quick-selection path is useful without numeric knobs. Preserve the
        // old requirement that a legacy parameter experiment declares at least one dimension.
        let continuous_ranges = self.search.as_ref().map_or(&[][..], |search| {
            search.continuous_parameter_space.as_slice()
        });
        let continuous_search = !continuous_ranges.is_empty();
        if continuous_search && (!self.parameter_space.is_empty() || strategy_selection) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "连续数值范围不能与离散 parameter_space 或 strategy_kinds 混用",
            ));
        }
        let per_kind = if continuous_search {
            let mut strategy = self.base.strategy.clone();
            strategy.kind = kinds[0];
            validate_continuous_grid(&strategy, continuous_ranges)?;
            1
        } else if self.parameter_space.is_empty() && strategy_selection {
            1
        } else {
            let mut maximum = 0;
            for kind in &kinds {
                let mut strategy = self.base.strategy.clone();
                strategy.kind = *kind;
                maximum = validate_grid(&strategy, &self.parameter_space)?;
            }
            maximum
        };
        let total = per_kind
            .checked_mul(kinds.len())
            .ok_or_else(|| AppError::new(AppErrorCategory::InvalidInput, "实验候选数溢出"))?;
        if total > MAX_EXPERIMENT_CANDIDATES {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("实验最多接受 {MAX_EXPERIMENT_CANDIDATES} 个候选组合"),
            ));
        }
        if !self.cost_sensitivity.is_empty() {
            if self.search.is_none() || self.split_plan.is_none() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "费用/滑点敏感性要求自适应搜索及训练/OOS切分",
                ));
            }
            if self.cost_sensitivity.len() > 16 {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "费用/滑点敏感性场景最多 16 个",
                ));
            }
            let mut names = BTreeSet::new();
            for scenario in &self.cost_sensitivity {
                if scenario.name.trim().is_empty()
                    || scenario.name.len() > 64
                    || !names.insert(scenario.name.as_str())
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "成本敏感性场景名称必须非空、唯一且不超过 64 字节",
                    ));
                }
                if scenario.execution_cost_rules.is_some() == scenario.ashare_rules.is_some() {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "成本情景必须且只能指定 execution_cost_rules 或 ashare_rules 之一",
                    ));
                }
                if scenario
                    .max_training_return_degradation_bps
                    .is_some_and(|value| value > 10_000)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "max_training_return_degradation_bps 必须在 0..=10000 基点内",
                    ));
                }
                if scenario
                    .max_neighborhood_training_return_degradation_bps
                    .is_some_and(|value| value > 10_000)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "max_neighborhood_training_return_degradation_bps 必须在 0..=10000 基点内",
                    ));
                }
                if let Some(rules) = &scenario.execution_cost_rules {
                    rules.validate().map_err(|error| {
                        AppError::new(AppErrorCategory::InvalidInput, error.to_string())
                    })?;
                    if self.base.ashare_rules.is_some() {
                        return Err(AppError::new(
                            AppErrorCategory::InvalidInput,
                            "A股基准回测不能使用通用 maker/taker 成本情景",
                        ));
                    }
                }
                if let Some(rules) = &scenario.ashare_rules {
                    rules
                        .validate()
                        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
                }
                if scenario.fill_model == crate::BacktestFillModel::OneTickSlippage
                    && self.base.market_spec.is_none()
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "one_tick_slippage 成本敏感性要求 market_spec",
                    ));
                }
            }
        }
        let search_budget = if let Some(search) = &self.search {
            if self.split_plan.is_none() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "随机/TPE 参数搜索必须配置 split_plan；候选选择只允许读取训练窗",
                ));
            }
            if (!continuous_search && self.parameter_space.is_empty())
                || !self.strategy_kinds.is_empty()
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "随机/TPE 搜索只接受参数维度，不接受空参数集或 strategy_kinds",
                ));
            }
            let search_space = if continuous_search {
                continuous_domain_cardinality(continuous_ranges).ok_or_else(|| {
                    AppError::new(AppErrorCategory::InvalidInput, "连续参数搜索值域无效")
                })?
            } else {
                total as u128
            };
            if search.budget == 0
                || search.budget > MAX_EXPERIMENT_CANDIDATES
                || search.budget as u128 > search_space
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "搜索 budget 必须在 1..={} 内，且不得超过参数空间可取的不同候选数",
                        MAX_EXPERIMENT_CANDIDATES
                    ),
                ));
            }
            if search
                .max_training_drawdown_bps
                .is_some_and(|value| value > 10_000)
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "max_training_drawdown_bps 必须在 0..=10000 基点内",
                ));
            }
            if search
                .neighborhood_radius
                .is_some_and(|radius| !(1..=8).contains(&radius))
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "neighborhood_radius 必须在 1..=8 内",
                ));
            }
            if let Some(subwindows) = search.robustness_subwindows {
                let train_bars = self
                    .split_plan
                    .as_ref()
                    .map(|plan| plan.train_bars)
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            "稳健性目标必须配置训练/OOS切分",
                        )
                    })?;
                let minimum_bars = u64::try_from(subwindows)
                    .ok()
                    .and_then(|count| count.checked_add(1));
                if !(2..=8).contains(&subwindows)
                    || minimum_bars.is_none_or(|minimum| train_bars < minimum)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "robustness_subwindows 必须在 2..=8 内，且训练窗至少包含子窗数 + 1 根 Bar",
                    ));
                }
            }
            if search.minimum_training_robustness_score_bps.is_some()
                && search.robustness_subwindows.is_none()
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "minimum_training_robustness_score_bps 要求配置 robustness_subwindows",
                ));
            }
            if let Some(pruning) = &search.early_pruning {
                let train_bars = self
                    .split_plan
                    .as_ref()
                    .map(|plan| plan.train_bars)
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            "早停剪枝必须配置训练窗切分计划",
                        )
                    })?;
                if search.max_training_drawdown_bps.is_none()
                    || pruning.min_training_bars < 2
                    || pruning.min_training_bars > train_bars
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "早停剪枝要求配置最大训练回撤预算，且 min_training_bars 必须在 2..=train_bars 内",
                    ));
                }
            }
            if matches!(
                search.algorithm,
                ExperimentSearchAlgorithm::Tpe
                    | ExperimentSearchAlgorithm::TpeKde
                    | ExperimentSearchAlgorithm::ParetoTpe
            ) && search.budget < 5
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "TPE 搜索 budget 至少为 5，前 4 个试验用于随机启动",
                ));
            }
            Some(search.budget)
        } else {
            if continuous_search {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "连续数值范围必须配置自适应搜索计划",
                ));
            }
            None
        };
        let mut base = self.base.clone();
        base.strategy.kind = kinds[0];
        base.strategy = normalized_strategy(&base.strategy, &self.parameter_space);
        base.validate()?;
        if let Some(plan) = &self.split_plan {
            plan.validate()
                .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
        }
        let mut seen_baselines = BTreeSet::new();
        for baseline in &self.baselines {
            if !EXPERIMENT_BASELINE_NAMES.contains(&baseline.as_str()) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "实验基准 {baseline} 不在 {} 内",
                        EXPERIMENT_BASELINE_NAMES.join(" / ")
                    ),
                ));
            }
            if !seen_baselines.insert(baseline) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("实验基准 {baseline} 重复声明"),
                ));
            }
        }
        Ok(search_budget.unwrap_or(total))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentCandidateResult {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<BacktestOutcome>,
    /// Stable AppError JSON for a failed candidate; a failure does not hide other candidates.
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruning: Option<ExperimentCandidatePruning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub spec_fingerprint: String,
    /// Path-independent identity of the experiment inputs. Legacy reports omit both fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_run_spec_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_run_spec_fingerprint: Option<String>,
    /// Identity of the actual BarFrame, including bars that are outside scoring windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_data_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_record: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_locations: Option<BTreeMap<String, String>>,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    #[serde(default)]
    pub pruned_candidates: usize,
    pub candidates: Vec<ExperimentCandidateResult>,
    pub comparison: Option<CompareRunsResult>,
    /// 样本外评估。spec 没写切分计划时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_of_sample: Option<ExperimentOutOfSampleReport>,
    /// 基准对照列。spec 没请求基准时为空表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baselines: Vec<ExperimentBaselineReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<ExperimentSearchReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cost_sensitivity: Vec<ExecutionCostSensitivityResult>,
    pub artifact_path: String,
}

impl RunExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let result: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("RunExperimentResult JSON 无法读取: {error}"),
            )
        })?;
        validate_experiment_run_identity(&result)?;
        validate_experiment_artifact_envelope(&result)?;
        Ok(result)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        validate_experiment_run_identity(self)?;
        validate_experiment_artifact_envelope(self)?;
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunExperimentResult 序列化失败: {error}"),
            )
        })
    }
}

/// Expand a bounded parameter grid, run each candidate through `run_backtest`, and persist a
/// deterministic report. Candidate errors are isolated and returned alongside successful runs.
pub fn run_experiment(
    spec: &RunExperimentSpec,
    context: &RunContext,
) -> Result<RunExperimentResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    context.require(CallerCapability::Research, "参数回测实验")?;
    guard_panics(&correlation_id, || {
        run_experiment_inner(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步参数实验未产生报告",
            ),
            &correlation_id,
        )
    })
}

impl RunExperimentSpec {
    /// Start an experiment with cancellation checks between candidates and
    /// inside each candidate's shared Rust Bar engine loop.
    pub fn start(self, context: RunContext) -> RunHandle<RunExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "参数回测实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_experiment_inner(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Start a bounded parameter experiment with a fresh caller-authored strategy per candidate.
    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunExperimentResult>, AppError>
    where
        F: FnMut(&BacktestSpec) -> Result<Box<dyn Strategy>, String> + Send + 'static,
    {
        context.require(CallerCapability::Research, "多语言策略参数回测实验")?;
        validate_strategy_experiment(&self, &identity)?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_experiment_inner(
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

fn validate_strategy_experiment(
    spec: &RunExperimentSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    spec.validate()?;
    identity.validate_for_experiment(&spec.base)?;
    if !spec.strategy_kinds.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "caller strategy factory cannot be combined with built-in strategy_kinds selection",
        ));
    }
    Ok(())
}

fn run_experiment_inner(
    spec: &RunExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunExperimentResult>, AppError> {
    let total = spec.validate()?;
    let spec_json = spec.to_json()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec_json);
    if let Some(identity) = identity {
        fingerprint.write_text(&identity.strategy_id);
        fingerprint.write_text(&identity.strategy_version);
        fingerprint.write_text(&identity.source_sha256);
    }
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let payload = read_text(&spec.base.bars_path)?;
    let input_frame = BarFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 校验失败 {}: {error:?}", spec.base.bars_path),
        )
    })?;
    let input_data_fingerprint = format!(
        "barframe:{}",
        qx_data::fingerprint_bar_frame(&input_frame).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BarFrame 内容指纹计算失败：{error}"),
            )
        })?
    );
    let canonical_run_spec = build_experiment_canonical_run_spec(
        spec,
        identity,
        &spec_json,
        &input_frame,
        &input_data_fingerprint,
        context,
    )?;
    let canonical_run_spec_json = canonical_run_spec.canonical_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验 CanonicalRunSpec 规范化失败: {error}"),
        )
    })?;
    let canonical_run_spec_fingerprint = canonical_run_spec.fingerprint().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验 CanonicalRunSpec 指纹失败: {error}"),
        )
    })?;
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let report_index_path = root
        .join(format!("{}.experiment.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    let spec_artifact_path = root
        .join(format!("{}.experiment.spec.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    let persisted_input = serialize_experiment_input(&spec_json, context.code_commit(), identity)?;
    if let Some(previous) =
        recovery::resume_existing_experiment(recovery::ExistingExperimentResume {
            report_index_path: &report_index_path,
            spec_artifact_path: &spec_artifact_path,
            persisted_input: &persisted_input,
            spec_fingerprint: &spec_fingerprint,
            canonical_run_spec: &canonical_run_spec,
            canonical_run_spec_json: &canonical_run_spec_json,
            canonical_run_spec_fingerprint: &canonical_run_spec_fingerprint,
            input_data_fingerprint: &input_data_fingerprint,
            experiment_id: &spec.experiment_id,
            root: &root,
            context,
        })?
    {
        return Ok(Some(previous));
    }
    // Split bounds and resource limits are preflighted before any candidate is run. Keep the
    // validated BarFrame for the report instead of reading it a second time after the grid.
    let (evaluation_bars, evaluation_frame) = if spec.split_plan.is_some()
        || !spec.baselines.is_empty()
    {
        let frame = input_frame;
        let bars = frame.bars().map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("实验 BarFrame exact OHLCV 无效: {error:?}"),
            )
        })?;
        if let Some(plan) = &spec.split_plan {
            let folds = experiment_folds(plan, bars.len())?;
            if let Some(search) = &spec.search {
                let neighborhood_cost_scenario_count = spec
                    .cost_sensitivity
                    .iter()
                    .filter(|scenario| {
                        scenario
                            .max_neighborhood_training_return_degradation_bps
                            .is_some()
                    })
                    .count();
                if search.fold_local_search
                    && fold_local_execution_count(
                        folds.len(),
                        search.budget,
                        spec.cost_sensitivity.len(),
                        neighborhood_cost_scenario_count,
                    )
                    .is_none_or(|runs| runs > MAX_FOLD_LOCAL_RUNS)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!(
                            "折内外搜索候选与成本情景合计最多允许 {MAX_FOLD_LOCAL_RUNS} 次回测"
                        ),
                    ));
                }
                if !search.fold_local_search
                    && search_execution_count(
                        search.budget,
                        spec.cost_sensitivity.len(),
                        neighborhood_cost_scenario_count,
                    )
                    .is_none_or(|runs| runs > MAX_FOLD_LOCAL_RUNS)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!(
                            "全局搜索候选与成本/邻域压力情景合计最多允许 {MAX_FOLD_LOCAL_RUNS} 次回测"
                        ),
                    ));
                }
            }
        }
        (Some(bars), Some(frame))
    } else {
        (None, None)
    };
    let pruning_window = match (spec.search.as_ref(), evaluation_bars.as_deref()) {
        (Some(search), Some(bars)) => search
            .early_pruning
            .as_ref()
            .zip(search.max_training_drawdown_bps)
            .map(|(early, max_drawdown_bps)| -> Result<_, AppError> {
                let split_plan = spec.split_plan.as_ref().ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "早停配置缺少 walk-forward split plan",
                    )
                })?;
                let folds = experiment_folds(split_plan, bars.len()).map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("早停窗口的 walk-forward folds 无效: {}", error.message()),
                    )
                })?;
                let training_bars = folds
                    .first()
                    .and_then(|fold| fold.train.get(1))
                    .copied()
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            "早停窗口缺少首折训练 Bar 范围",
                        )
                    })?;
                Ok(DrawdownPruningWindow {
                    min_bars: usize::try_from(early.min_training_bars).map_err(|_| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            "早停最小训练 Bar 数超过平台上限",
                        )
                    })?,
                    training_bars,
                    max_drawdown_bps,
                })
            })
            .transpose()?,
        _ => None,
    };
    let candidate_execution =
        candidate_execution::execute_candidates(candidate_execution::CandidateExecutionRequest {
            spec,
            total,
            root: &root,
            context,
            cancellation,
            identity,
            factory: factory.as_deref_mut(),
            spec_fingerprint: &spec_fingerprint,
            input_data_fingerprint: &input_data_fingerprint,
            evaluation_bars: evaluation_bars.as_deref(),
            pruning_window,
        })?;
    let Some(candidate_execution) = candidate_execution else {
        return Ok(None);
    };
    let candidate_execution::CandidateExecutionResult {
        combinations,
        candidates,
        search_trace,
    } = candidate_execution;

    let cost_sensitivity =
        cost_sensitivity::execute_cost_sensitivity(cost_sensitivity::CostSensitivityRequest {
            spec,
            context,
            cancellation,
            identity,
            factory: factory.as_deref_mut(),
            candidates: &candidates,
            search_trace: &search_trace,
            combinations: &combinations,
            root: &root,
            spec_fingerprint: &spec_fingerprint,
            input_data_fingerprint: &input_data_fingerprint,
            evaluation_bar_count: evaluation_bars.as_ref().map(Vec::len),
        })?;
    let Some(cost_sensitivity) = cost_sensitivity else {
        return Ok(None);
    };

    assemble_and_persist(ExperimentResultAssemblyInput {
        spec,
        context,
        cancellation,
        identity,
        factory,
        evaluation_bars,
        evaluation_frame,
        root,
        spec_fingerprint,
        input_data_fingerprint,
        combinations,
        total,
        candidates,
        search_trace,
        cost_sensitivity,
        canonical_run_spec,
        canonical_run_spec_json,
        canonical_run_spec_fingerprint,
        persisted_input,
        report_index_path,
        spec_artifact_path,
    })
}

fn experiment_storage_error(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}
