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

use super::artifacts::{
    artifact_paths, equity_series, persist_run_manifest, read_text, write_text,
};
use super::compare_runs::COMPARE_RUNS_SCHEMA_VERSION;
use super::experiment_fold_search::{
    artifact_fingerprint, evaluate_fold_local_search, fold_local_execution_count,
    load_cost_scenario_checkpoint, search_execution_count, write_cost_scenario_checkpoint,
    MAX_FOLD_LOCAL_RUNS,
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
use super::experiment_spec_artifact::{
    serialize_experiment_input, verify_spec_artifact, write_spec_artifact,
};
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
use qx_spec::{
    ArtifactRefV2, CanonicalDatasetIdentity, CanonicalRunClock, CanonicalRunSpec,
    CanonicalStrategyIdentity, ReplayVerdict, RunRecordV2, RunStatus,
};
use qx_strategy::BuiltinStrategyKind;
use qx_strategy::Strategy;
use qx_xingban::{AshareRuleConfig, ExecutionCostRules};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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

pub(super) fn cost_sensitivity_fold_delta(
    fold: usize,
    train_range: [usize; 2],
    test_range: [usize; 2],
    series_offset: usize,
    baseline: &[i128],
    stressed: &[i128],
) -> ExecutionCostSensitivityFoldDelta {
    let relative = |range: [usize; 2]| {
        debug_assert!(range[0] >= series_offset && range[1] >= series_offset);
        [range[0] - series_offset, range[1] - series_offset]
    };
    let return_delta = |range: [usize; 2]| {
        let range = relative(range);
        i64::from(window_return_bps(stressed, range[0], range[1]))
            - i64::from(window_return_bps(baseline, range[0], range[1]))
    };
    let drawdown_delta = |range: [usize; 2]| {
        let range = relative(range);
        i64::from(window_max_drawdown_bps(stressed, range[0], range[1]))
            - i64::from(window_max_drawdown_bps(baseline, range[0], range[1]))
    };
    ExecutionCostSensitivityFoldDelta {
        fold,
        train_range: [train_range[0] as u64, train_range[1] as u64],
        test_range: [test_range[0] as u64, test_range[1] as u64],
        training_return_delta_bps: return_delta(train_range),
        training_drawdown_delta_bps: drawdown_delta(train_range),
        out_of_sample_return_delta_bps: return_delta(test_range),
        out_of_sample_drawdown_delta_bps: drawdown_delta(test_range),
    }
}

pub(super) fn cost_robustness_passed(
    threshold_bps: Option<u32>,
    fold_deltas: &[ExecutionCostSensitivityFoldDelta],
    stress_completed: bool,
) -> Option<bool> {
    threshold_bps.map(|threshold| {
        stress_completed
            && !fold_deltas.is_empty()
            && fold_deltas
                .iter()
                .all(|fold| fold.training_return_delta_bps >= -i64::from(threshold))
    })
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

#[derive(Serialize, Deserialize)]
struct SearchCandidateCheckpoint {
    spec_fingerprint: String,
    input_data_fingerprint: String,
    ordinal: usize,
    candidate_index: usize,
    proposal: String,
    strategy_kind: String,
    parameters: BTreeMap<String, i128>,
    outcome: BacktestOutcome,
    artifact_fingerprint: String,
    training_score_bps: i32,
    training_max_drawdown_bps: u32,
    #[serde(default)]
    training_robustness_score_bps: Option<i32>,
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
    match std::fs::read_to_string(&report_index_path) {
        Ok(existing) => {
            let mut previous = existing_result(
                &existing,
                &spec_fingerprint,
                &canonical_run_spec_fingerprint,
                &input_data_fingerprint,
                &spec.experiment_id,
            )?;
            if !has_retryable_search_failures(&previous) {
                previous.input_data_fingerprint = Some(input_data_fingerprint.clone());
                previous.canonical_run_spec_json = Some(canonical_run_spec_json.clone());
                previous.canonical_run_spec_fingerprint =
                    Some(canonical_run_spec_fingerprint.clone());
                verify_experiment_candidate_outcomes(&previous, context)?;
                if previous.run_manifest.is_none()
                    && previous.run_record.is_none()
                    && previous.result_hash.is_none()
                    && previous.artifact_locations.is_none()
                {
                    write_spec_artifact(&spec_artifact_path, &persisted_input, "参数实验")?;
                    let upgraded = persist_experiment_result(
                        previous,
                        &canonical_run_spec,
                        &root,
                        &report_index_path,
                        &spec_artifact_path,
                    )?;
                    return Ok(Some(upgraded));
                }
                verify_persisted_experiment_result(&previous)?;
                let has_input_artifact = previous
                    .artifact_locations
                    .as_ref()
                    .and_then(|locations| locations.get("experiment_input"))
                    .is_some_and(|path| path == &spec_artifact_path);
                if has_input_artifact {
                    verify_spec_artifact(&spec_artifact_path, &persisted_input, "参数实验")?;
                } else {
                    write_spec_artifact(&spec_artifact_path, &persisted_input, "参数实验")?;
                    let upgraded = persist_experiment_result(
                        previous,
                        &canonical_run_spec,
                        &root,
                        &report_index_path,
                        &spec_artifact_path,
                    )?;
                    return Ok(Some(upgraded));
                }
                return Ok(Some(previous));
            }
            // Successful candidate/scenario checkpoints remain the source of truth. A prior
            // report containing failures is resumable: entries without verified success
            // checkpoints are executed again and the refreshed report replaces the old one.
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有实验报告", &error)),
    }
    // Split bounds and resource limits are preflighted before any candidate is run. Keep the
    // validated BarFrame for the report instead of reading it a second time after the grid.
    let (evaluation_bars, evaluation_frame) = if spec.split_plan.is_some()
        || !spec.baselines.is_empty()
    {
        let frame = input_frame;
        let bars: Vec<_> = (&frame).into();
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
            .map(|(early, max_drawdown_bps)| {
                let training_bars = experiment_folds(
                    spec.split_plan.as_ref().expect("validated search split"),
                    bars.len(),
                )
                .expect("split plan preflighted above")[0]
                    .train[1];
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
    let candidate_pairs = kinds
        .into_iter()
        .flat_map(|kind| {
            combinations
                .iter()
                .cloned()
                .map(move |parameters| (kind, parameters))
        })
        .collect::<Vec<_>>();
    if spec.search.is_none() {
        debug_assert_eq!(candidate_pairs.len(), total);
    }

    let mut candidates = Vec::with_capacity(total);
    let mut search_trace: Vec<ExperimentSearchTrial> =
        Vec::with_capacity(spec.search.as_ref().map_or(0, |plan| plan.budget));
    let mut observed_training_scores = Vec::new();
    let mut observed_training_drawdowns = Vec::new();
    let mut search_state = spec.search.as_ref().and_then(|plan| {
        plan.continuous_parameter_space
            .is_empty()
            .then(|| SearchState::new(combinations.len(), plan.seed))
    });
    let mut continuous_search_state = spec.search.as_ref().and_then(|plan| {
        (!plan.continuous_parameter_space.is_empty())
            .then(|| ContinuousSearchState::new(&plan.continuous_parameter_space, plan.seed))
    });
    for index in 0..total {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let ordinal = index + 1;
        let (kind, parameters, proposal, candidate_index) = if let Some(plan) = &spec.search {
            if !plan.continuous_parameter_space.is_empty() {
                let eligible_trials = search_trace
                    .iter()
                    .filter(|trial| {
                        plan.max_training_drawdown_bps.is_none_or(|limit| {
                            trial
                                .training_max_drawdown_bps
                                .is_some_and(|drawdown| drawdown <= limit)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let (parameters, proposal) = continuous_search_state
                    .as_mut()
                    .and_then(|state| {
                        state.next(
                            plan.algorithm,
                            if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                                ExperimentSearchObjective::ParetoReturnDrawdown
                            } else {
                                ExperimentSearchObjective::Return
                            },
                            4,
                            &plan.continuous_parameter_space,
                            &eligible_trials,
                        )
                    })
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "连续搜索未能提出不同候选；值域计数与 budget 不一致",
                        )
                    })?;
                (
                    spec.base.strategy.kind,
                    parameters,
                    Some(proposal.to_string()),
                    Some(ordinal - 1),
                )
            } else {
                let (candidate_index, proposal) = search_state
                    .as_mut()
                    .and_then(|state| {
                        state.next_with_objective(
                            plan.algorithm,
                            if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                                ExperimentSearchObjective::ParetoReturnDrawdown
                            } else {
                                ExperimentSearchObjective::Return
                            },
                            4,
                            &spec.parameter_space,
                            &combinations,
                            &if let Some(limit) = plan.max_training_drawdown_bps {
                                observed_training_scores
                                    .iter()
                                    .copied()
                                    .filter(|(index, _)| {
                                        observed_training_drawdowns.iter().any(
                                            |(drawdown_index, dd)| {
                                                drawdown_index == index && *dd <= limit
                                            },
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                observed_training_scores.clone()
                            },
                            &if let Some(limit) = plan.max_training_drawdown_bps {
                                observed_training_drawdowns
                                    .iter()
                                    .copied()
                                    .filter(|(_, dd)| *dd <= limit)
                                    .collect::<Vec<_>>()
                            } else {
                                observed_training_drawdowns.clone()
                            },
                        )
                    })
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "搜索候选提前耗尽，budget 与有限候选空间不一致",
                        )
                    })?;
                (
                    spec.base.strategy.kind,
                    combinations[candidate_index].clone(),
                    Some(proposal.to_string()),
                    Some(candidate_index),
                )
            }
        } else {
            let (kind, parameters) = candidate_pairs.get(index).cloned().ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "实验候选数量与组合数量不一致",
                )
            })?;
            (kind, parameters, None, None)
        };
        let mut candidate = spec.base.clone();
        candidate.strategy.kind = kind;
        candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
        candidate.output_dir = root
            .join(format!("run-{ordinal:04}"))
            .to_string_lossy()
            .into();
        let checkpoint_path = proposal
            .as_ref()
            .map(|_| root.join(format!("search-trial-{ordinal:04}.checkpoint.json")));
        let mut cached_checkpoint = None;
        let attempt = match apply_parameters(&mut candidate.strategy, &parameters) {
            Ok(()) => {
                if let (Some(proposal), Some(candidate_index), Some(checkpoint_path)) = (
                    proposal.as_deref(),
                    candidate_index,
                    checkpoint_path.as_deref(),
                ) {
                    cached_checkpoint = load_search_candidate_checkpoint(
                        checkpoint_path,
                        &spec_fingerprint,
                        &input_data_fingerprint,
                        ordinal,
                        candidate_index,
                        proposal,
                        kind.name(),
                        &parameters,
                        &candidate,
                        context,
                    )?;
                }
                if let Some(checkpoint) = &cached_checkpoint {
                    Ok(SearchBacktestAttempt::Completed(Some(
                        checkpoint.outcome.clone(),
                    )))
                } else {
                    let run = match (identity, factory.as_deref_mut()) {
                        (Some(identity), Some(factory)) => factory(&candidate)
                            .map_err(|error| {
                                AppError::new(
                                    AppErrorCategory::InvalidInput,
                                    format!("候选 {ordinal} 自定义策略 factory 失败: {error}"),
                                )
                            })
                            .and_then(|strategy| {
                                run_backtest_inner_with_experiment_strategy(
                                    &candidate,
                                    context,
                                    cancellation,
                                    identity.clone(),
                                    strategy,
                                    pruning_window,
                                )
                            }),
                        (None, None) => run_backtest_inner_for_experiment(
                            &candidate,
                            context,
                            cancellation,
                            Vec::new(),
                            pruning_window,
                        ),
                        _ => Err(AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "自定义策略实验身份与 factory 状态不一致",
                        )),
                    };
                    run
                }
            }
            Err(error) => Err(error),
        };
        let (candidate_result, training_metrics) = match attempt {
            Ok(SearchBacktestAttempt::Completed(Some(outcome))) => {
                let training_metrics = if spec.search.is_some() {
                    let bars = evaluation_bars.as_deref().ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "adaptive search lost its validated BarFrame",
                        )
                    })?;
                    let payload = read_text(&outcome.artifacts.equity)?;
                    let values = equity_series(&payload).ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::StorageFailure,
                            format!("搜索候选权益曲线产物无法解析: {}", outcome.artifacts.equity),
                        )
                    })?;
                    let split = spec.split_plan.as_ref().ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "adaptive search lost its validated split plan",
                        )
                    })?;
                    let (score, drawdown) =
                        training_metrics_for_equity(split, bars.len(), &values)?;
                    let robustness_score = spec
                        .search
                        .as_ref()
                        .and_then(|plan| plan.robustness_subwindows)
                        .map(|count| {
                            let ranges = experiment_folds(split, bars.len())
                                .and_then(|folds| {
                                    folds.first().copied().ok_or_else(|| {
                                        AppError::new(
                                            AppErrorCategory::InternalInvariant,
                                            "adaptive search has no training fold",
                                        )
                                    })
                                })?;
                            window_robustness_score_bps(
                                &values,
                                ranges.train[0],
                                ranges.train[1],
                                count,
                            )
                            .ok_or_else(|| {
                                AppError::new(
                                    AppErrorCategory::InternalInvariant,
                                    "robustness subwindows do not fit the validated training window",
                                )
                            })
                        })
                        .transpose()?;
                    Some((score, drawdown, robustness_score))
                } else {
                    None
                };
                if let Some((
                    training_score_bps,
                    training_max_drawdown_bps,
                    training_robustness_score_bps,
                )) = training_metrics
                {
                    if let Some(checkpoint) = &cached_checkpoint {
                        if checkpoint.training_score_bps != training_score_bps
                            || checkpoint.training_max_drawdown_bps != training_max_drawdown_bps
                            || checkpoint.training_robustness_score_bps
                                != training_robustness_score_bps
                        {
                            return Err(AppError::new(
                                AppErrorCategory::StorageFailure,
                                format!("候选 {ordinal} 检查点训练指标与权益产物不一致"),
                            ));
                        }
                    } else if let (Some(proposal), Some(candidate_index), Some(checkpoint_path)) = (
                        proposal.as_ref(),
                        candidate_index,
                        checkpoint_path.as_deref(),
                    ) {
                        let checkpoint = SearchCandidateCheckpoint {
                            spec_fingerprint: spec_fingerprint.clone(),
                            input_data_fingerprint: input_data_fingerprint.clone(),
                            ordinal,
                            candidate_index,
                            proposal: proposal.to_string(),
                            strategy_kind: kind.name().to_string(),
                            parameters: parameters.clone(),
                            artifact_fingerprint: artifact_fingerprint(&outcome)?,
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
                        write_text(&checkpoint_path.to_string_lossy(), &payload)?;
                    }
                }
                (
                    ExperimentCandidateResult {
                        ordinal,
                        strategy_kind: (!spec.strategy_kinds.is_empty())
                            .then_some(kind.name().to_string()),
                        parameters: parameters.clone(),
                        outcome: Some(outcome),
                        error: None,
                        pruning: None,
                    },
                    training_metrics,
                )
            }
            Ok(SearchBacktestAttempt::Completed(None)) => return Ok(None),
            Ok(SearchBacktestAttempt::Pruned(pruning)) => (
                ExperimentCandidateResult {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then_some(kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: None,
                    error: None,
                    pruning: Some(pruning),
                },
                None,
            ),
            Err(error) => (
                ExperimentCandidateResult {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then_some(kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: None,
                    error: Some(error.to_json()),
                    pruning: None,
                },
                None,
            ),
        };
        let training_score_bps = training_metrics.map(|metrics| metrics.0);
        let training_max_drawdown_bps = training_metrics.map(|metrics| metrics.1);
        let training_robustness_score_bps = training_metrics.and_then(|metrics| metrics.2);
        if let (Some(proposal), Some(candidate_index)) = (proposal, candidate_index) {
            search_trace.push(ExperimentSearchTrial {
                ordinal,
                proposal,
                parameters: parameters.clone(),
                training_score_bps,
                training_max_drawdown_bps,
                training_robustness_score_bps,
            });
            if let Some(score) = training_robustness_score_bps.or(training_score_bps) {
                observed_training_scores.push((candidate_index, score));
            }
            if let Some(drawdown) = training_max_drawdown_bps {
                observed_training_drawdowns.push((candidate_index, drawdown));
            }
        }
        candidates.push(candidate_result);
    }

    let mut cost_sensitivity = Vec::with_capacity(spec.cost_sensitivity.len());
    if !spec.cost_sensitivity.is_empty() {
        let selected = search_trace
            .iter()
            .filter(|trial| {
                trial.selection_score_bps().is_some()
                    && spec
                        .search
                        .as_ref()
                        .and_then(|search| search.minimum_training_robustness_score_bps)
                        .is_none_or(|minimum| {
                            trial
                                .training_robustness_score_bps
                                .is_some_and(|score| score >= minimum)
                        })
                    && spec
                        .search
                        .as_ref()
                        .and_then(|search| search.max_training_drawdown_bps)
                        .is_none_or(|limit| {
                            trial
                                .training_max_drawdown_bps
                                .is_some_and(|dd| dd <= limit)
                        })
            })
            .max_by(|left, right| {
                left.selection_score_bps()
                    .cmp(&right.selection_score_bps())
                    .then_with(|| {
                        right
                            .training_max_drawdown_bps
                            .cmp(&left.training_max_drawdown_bps)
                    })
                    .then_with(|| right.ordinal.cmp(&left.ordinal))
            })
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "没有满足训练稳健收益与最大训练回撤门槛的成功候选，无法执行成本敏感性",
                )
            })?;
        let baseline_outcome = candidates
            .iter()
            .find(|candidate| candidate.ordinal == selected.ordinal)
            .and_then(|candidate| candidate.outcome.as_ref())
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "成本敏感性选中候选缺少已完成的基线回测",
                )
            })?;
        let baseline_payload = read_text(&baseline_outcome.artifacts.equity)?;
        let baseline_values = equity_series(&baseline_payload).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                "成本敏感性基线权益曲线无法解析",
            )
        })?;
        let split = spec.split_plan.as_ref().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "成本敏感性验证后缺少切分计划",
            )
        })?;
        let total_bars = evaluation_bars.as_ref().map(Vec::len).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "成本敏感性缺少已验证的 Bar 计数",
            )
        })?;
        if baseline_values.len() != total_bars || baseline_values.iter().any(|value| *value <= 0) {
            return Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                "成本敏感性基线权益点数与 Bar 数不一致或包含非正权益",
            ));
        }
        let sensitivity_folds = experiment_folds(split, total_bars)?;
        for (scenario_index, scenario) in spec.cost_sensitivity.iter().enumerate() {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let mut candidate = spec.base.clone();
            candidate.strategy.kind = spec.base.strategy.kind;
            apply_parameters(&mut candidate.strategy, &selected.parameters)?;
            candidate.run_id = format!("{}-cost-{:02}", spec.experiment_id, scenario_index + 1);
            candidate.output_dir = root
                .join("cost-sensitivity")
                .join(format!("scenario-{:02}", scenario_index + 1))
                .to_string_lossy()
                .into_owned();
            candidate.fee_schedule = None;
            candidate.execution_cost_rules = scenario.execution_cost_rules.clone();
            candidate.ashare_rules = scenario.ashare_rules.clone();
            candidate.fill_model = Some(scenario.fill_model);
            let checkpoint_path = root.join("cost-sensitivity").join(format!(
                "scenario-{:02}.checkpoint.json",
                scenario_index + 1
            ));
            let cached_outcome = load_cost_scenario_checkpoint(
                &checkpoint_path,
                &spec_fingerprint,
                &input_data_fingerprint,
                0,
                selected.ordinal,
                scenario_index + 1,
                &scenario.name,
                &selected.parameters,
                &candidate,
                context,
            )?;
            let reused_checkpoint = cached_outcome.is_some();
            let outcome = if let Some(outcome) = cached_outcome {
                Ok(Some(outcome))
            } else {
                match (identity, factory.as_deref_mut()) {
                    (Some(identity), Some(factory)) => factory(&candidate)
                        .map_err(|error| {
                            AppError::new(
                                AppErrorCategory::InvalidInput,
                                format!("成本情景策略 factory 失败: {error}"),
                            )
                        })
                        .and_then(|strategy| {
                            run_backtest_inner_with_experiment_strategy(
                                &candidate,
                                context,
                                cancellation,
                                identity.clone(),
                                strategy,
                                None,
                            )
                            .and_then(|attempt| match attempt {
                                SearchBacktestAttempt::Completed(outcome) => Ok(outcome),
                                SearchBacktestAttempt::Pruned(_) => Err(AppError::new(
                                    AppErrorCategory::InternalInvariant,
                                    "未启用早停的成本情景被意外剪枝",
                                )),
                            })
                        }),
                    (None, None) => run_backtest_inner(&candidate, context, cancellation),
                    _ => Err(AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "成本情景自定义策略身份与 factory 状态不一致",
                    )),
                }
            };
            let outcome = match outcome {
                Ok(Some(outcome)) => {
                    if !reused_checkpoint {
                        write_cost_scenario_checkpoint(
                            &checkpoint_path,
                            &spec_fingerprint,
                            &input_data_fingerprint,
                            0,
                            selected.ordinal,
                            scenario_index + 1,
                            &scenario.name,
                            &selected.parameters,
                            &outcome,
                        )?;
                    }
                    (Some(outcome), None)
                }
                Ok(None) => return Ok(None),
                Err(error) => (None, Some(error.to_json())),
            };
            let baseline = candidates
                .iter()
                .find(|candidate| candidate.ordinal == selected.ordinal)
                .and_then(|candidate| candidate.outcome.as_ref());
            let return_delta_bps = baseline
                .zip(outcome.0.as_ref())
                .map(|(baseline, stressed)| {
                    i64::from(stressed.return_bps) - i64::from(baseline.return_bps)
                });
            let drawdown_delta_bps =
                baseline
                    .zip(outcome.0.as_ref())
                    .map(|(baseline, stressed)| {
                        i64::from(stressed.max_drawdown_bps) - i64::from(baseline.max_drawdown_bps)
                    });
            let fold_deltas = if let Some(stressed) = outcome.0.as_ref() {
                let stressed_payload = read_text(&stressed.artifacts.equity)?;
                let stressed_values = equity_series(&stressed_payload).ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::StorageFailure,
                        "成本敏感性权益曲线无法解析",
                    )
                })?;
                if stressed_values.len() != total_bars
                    || stressed_values.iter().any(|value| *value <= 0)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "成本敏感性权益点数与 Bar 数不一致或包含非正权益",
                    ));
                }
                sensitivity_folds
                    .iter()
                    .enumerate()
                    .map(|(index, ranges)| {
                        cost_sensitivity_fold_delta(
                            index + 1,
                            ranges.train,
                            ranges.test,
                            0,
                            &baseline_values,
                            &stressed_values,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let selected_cost_robustness_passed = cost_robustness_passed(
                scenario.max_training_return_degradation_bps,
                &fold_deltas,
                outcome.0.is_some(),
            );
            let neighborhood_cost_robustness = if let Some(threshold_bps) =
                scenario.max_neighborhood_training_return_degradation_bps
            {
                let search = spec.search.as_ref().expect("validated search plan");
                let radius = search.neighborhood_radius.unwrap_or(1);
                let neighbor_parameters = neighborhood_parameter_sets(
                    &spec.parameter_space,
                    &combinations,
                    &search.continuous_parameter_space,
                    &selected.parameters,
                    radius,
                );
                let mut neighbor_results = Vec::new();
                for parameters in &neighbor_parameters {
                    let Some(trial) = search_trace
                        .iter()
                        .find(|trial| trial.parameters == *parameters)
                    else {
                        continue;
                    };
                    let neighbor_candidate = candidates
                        .iter()
                        .find(|candidate| candidate.ordinal == trial.ordinal);
                    let Some(neighbor_baseline) =
                        neighbor_candidate.and_then(|candidate| candidate.outcome.as_ref())
                    else {
                        neighbor_results.push(ExecutionCostSensitivityNeighborResult {
                            ordinal: trial.ordinal,
                            parameters: trial.parameters.clone(),
                            fold_deltas: Vec::new(),
                            cost_robustness_passed: false,
                            outcome: None,
                            error: neighbor_candidate.and_then(|candidate| candidate.error.clone()),
                        });
                        continue;
                    };
                    let neighbor_baseline_values =
                        equity_series(&read_text(&neighbor_baseline.artifacts.equity)?)
                            .ok_or_else(|| {
                                AppError::new(
                                    AppErrorCategory::StorageFailure,
                                    "成本敏感性邻域基线权益曲线无法解析",
                                )
                            })?;
                    if neighbor_baseline_values.len() != total_bars
                        || neighbor_baseline_values.iter().any(|value| *value <= 0)
                    {
                        return Err(AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "成本敏感性邻域基线权益点数与 Bar 数不一致或包含非正权益",
                        ));
                    }
                    let mut neighbor = spec.base.clone();
                    neighbor.strategy.kind = spec.base.strategy.kind;
                    apply_parameters(&mut neighbor.strategy, parameters)?;
                    neighbor.run_id = format!(
                        "{}-cost-{:02}-neighbor-{:04}",
                        spec.experiment_id,
                        scenario_index + 1,
                        trial.ordinal
                    );
                    neighbor.output_dir = root
                        .join("cost-sensitivity")
                        .join(format!("scenario-{:02}", scenario_index + 1))
                        .join("neighbors")
                        .join(format!("candidate-{:04}", trial.ordinal))
                        .to_string_lossy()
                        .into_owned();
                    neighbor.fee_schedule = None;
                    neighbor.execution_cost_rules = scenario.execution_cost_rules.clone();
                    neighbor.ashare_rules = scenario.ashare_rules.clone();
                    neighbor.fill_model = Some(scenario.fill_model);
                    let checkpoint_path = root
                        .join("cost-sensitivity")
                        .join(format!("scenario-{:02}", scenario_index + 1))
                        .join("neighbors")
                        .join(format!("candidate-{:04}.checkpoint.json", trial.ordinal));
                    let cached = load_cost_scenario_checkpoint(
                        &checkpoint_path,
                        &spec_fingerprint,
                        &input_data_fingerprint,
                        0,
                        trial.ordinal,
                        scenario_index + 1,
                        &scenario.name,
                        parameters,
                        &neighbor,
                        context,
                    )?;
                    let reused = cached.is_some();
                    let run = if let Some(cached) = cached {
                        Ok(Some(cached))
                    } else {
                        match (identity, factory.as_deref_mut()) {
                            (Some(identity), Some(factory)) => factory(&neighbor)
                                .map_err(|error| {
                                    AppError::new(
                                        AppErrorCategory::InvalidInput,
                                        format!("成本敏感性邻域策略 factory 失败: {error}"),
                                    )
                                })
                                .and_then(|strategy| {
                                    run_backtest_inner_with_experiment_strategy(
                                        &neighbor,
                                        context,
                                        cancellation,
                                        identity.clone(),
                                        strategy,
                                        None,
                                    )
                                    .and_then(|attempt| {
                                        match attempt {
                                            SearchBacktestAttempt::Completed(outcome) => {
                                                Ok(outcome)
                                            }
                                            SearchBacktestAttempt::Pruned(_) => Err(AppError::new(
                                                AppErrorCategory::InternalInvariant,
                                                "未启用早停的邻域成本情景被意外剪枝",
                                            )),
                                        }
                                    })
                                }),
                            (None, None) => run_backtest_inner(&neighbor, context, cancellation),
                            _ => Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                "成本情景自定义策略身份与 factory 状态不一致",
                            )),
                        }
                    };
                    let stressed = match run {
                        Ok(Some(stressed)) => {
                            if !reused {
                                write_cost_scenario_checkpoint(
                                    &checkpoint_path,
                                    &spec_fingerprint,
                                    &input_data_fingerprint,
                                    0,
                                    trial.ordinal,
                                    scenario_index + 1,
                                    &scenario.name,
                                    parameters,
                                    &stressed,
                                )?;
                            }
                            (Some(stressed), None)
                        }
                        Ok(None) => return Ok(None),
                        Err(error) => (None, Some(error.to_json())),
                    };
                    let neighbor_deltas = if let Some(stressed) = stressed.0.as_ref() {
                        let stressed_values = equity_series(&read_text(
                            &stressed.artifacts.equity,
                        )?)
                        .ok_or_else(|| {
                            AppError::new(
                                AppErrorCategory::StorageFailure,
                                "成本敏感性邻域压力权益曲线无法解析",
                            )
                        })?;
                        if stressed_values.len() != total_bars
                            || stressed_values.iter().any(|value| *value <= 0)
                        {
                            return Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                "成本敏感性邻域压力权益点数与 Bar 数不一致或包含非正权益",
                            ));
                        }
                        sensitivity_folds
                            .iter()
                            .enumerate()
                            .map(|(index, ranges)| {
                                cost_sensitivity_fold_delta(
                                    index + 1,
                                    ranges.train,
                                    ranges.test,
                                    0,
                                    &neighbor_baseline_values,
                                    &stressed_values,
                                )
                            })
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    let passed = cost_robustness_passed(
                        Some(threshold_bps),
                        &neighbor_deltas,
                        stressed.0.is_some(),
                    ) == Some(true);
                    neighbor_results.push(ExecutionCostSensitivityNeighborResult {
                        ordinal: trial.ordinal,
                        parameters: trial.parameters.clone(),
                        fold_deltas: neighbor_deltas,
                        cost_robustness_passed: passed,
                        outcome: stressed.0,
                        error: stressed.1,
                    });
                }
                let evaluated_neighbors = neighbor_results.len();
                let successful_neighbors = neighbor_results
                    .iter()
                    .filter(|candidate| candidate.outcome.is_some())
                    .count();
                let passed_neighbors = neighbor_results
                    .iter()
                    .filter(|candidate| candidate.cost_robustness_passed)
                    .count();
                let complete = evaluated_neighbors == neighbor_parameters.len();
                let passed = !neighbor_parameters.is_empty()
                    && complete
                    && successful_neighbors == neighbor_parameters.len()
                    && passed_neighbors == neighbor_parameters.len();
                Some(ExecutionCostSensitivityNeighborhoodResult {
                    radius,
                    possible_neighbors: neighbor_parameters.len(),
                    evaluated_neighbors,
                    successful_neighbors,
                    passed_neighbors,
                    complete,
                    passed,
                    candidates: neighbor_results,
                })
            } else {
                None
            };
            cost_sensitivity.push(ExecutionCostSensitivityResult {
                selected_ordinal: selected.ordinal,
                name: scenario.name.clone(),
                execution_cost_rules: scenario.execution_cost_rules.clone(),
                ashare_rules: scenario.ashare_rules.clone(),
                fill_model: scenario.fill_model,
                return_delta_bps,
                drawdown_delta_bps,
                max_training_return_degradation_bps: scenario.max_training_return_degradation_bps,
                cost_robustness_passed: selected_cost_robustness_passed,
                max_neighborhood_training_return_degradation_bps: scenario
                    .max_neighborhood_training_return_degradation_bps,
                neighborhood_cost_robustness,
                fold_deltas,
                outcome: outcome.0,
                error: outcome.1,
            });
        }
    }

    let successful: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| candidate.outcome.as_ref())
        .collect();
    let comparison = if successful.len() >= 2 {
        let runs = successful
            .iter()
            .map(|outcome| ComparedRun {
                run_id: outcome.run_id.clone(),
                instrument: outcome.instrument.clone(),
                data_fingerprint: outcome.data_fingerprint.clone(),
                result_hash: outcome.result_hash.clone(),
                return_bps: outcome.return_bps,
                max_drawdown_bps: outcome.max_drawdown_bps,
            })
            .collect();
        Some(compare_runs(
            &CompareRunsSpec {
                schema_version: COMPARE_RUNS_SCHEMA_VERSION,
                runs,
            },
            context,
        )?)
    } else {
        None
    };
    // 切分与基准都要 Bar 序列本身（基准用收盘价、切分用样本根数），所以只在**真被请求**时
    // 才多读一次数据文件——没写这两格的 spec 走的还是原来那条路，一次多余 IO 都没有。
    let (out_of_sample, baselines, fold_searches) = if let Some(bars) = evaluation_bars.as_deref() {
        let baselines = evaluate_baselines(bars, &spec.baselines);
        let out_of_sample = match &spec.split_plan {
            Some(plan) => {
                if spec
                    .search
                    .as_ref()
                    .is_some_and(|search| search.fold_local_search)
                {
                    let frame = evaluation_frame.as_ref().ok_or_else(|| {
                        AppError::new(AppErrorCategory::InternalInvariant, "折内搜索缺少 BarFrame")
                    })?;
                    let result = evaluate_fold_local_search(
                        spec,
                        frame,
                        &combinations,
                        &root,
                        &spec_fingerprint,
                        &input_data_fingerprint,
                        context,
                        cancellation,
                        identity,
                        factory,
                    )?;
                    let Some((report, searches)) = result else {
                        return Ok(None);
                    };
                    (report, searches)
                } else {
                    let drawdown_limit = spec
                        .search
                        .as_ref()
                        .and_then(|search| search.max_training_drawdown_bps);
                    let has_candidate_for_oos = candidates.iter().any(|candidate| {
                        candidate.outcome.is_some()
                            && spec
                                .search
                                .as_ref()
                                .and_then(|search| search.minimum_training_robustness_score_bps)
                                .is_none_or(|minimum| {
                                    search_trace
                                        .iter()
                                        .find(|trial| trial.ordinal == candidate.ordinal)
                                        .and_then(|trial| trial.training_robustness_score_bps)
                                        .is_some_and(|score| score >= minimum)
                                })
                            && drawdown_limit.is_none_or(|limit| {
                                search_trace
                                    .iter()
                                    .find(|trial| trial.ordinal == candidate.ordinal)
                                    .and_then(|trial| trial.training_max_drawdown_bps)
                                    .is_some_and(|drawdown| drawdown <= limit)
                            })
                    });
                    if !has_candidate_for_oos {
                        (None, Vec::new())
                    } else {
                        let report = evaluate_out_of_sample(
                            plan,
                            bars.len(),
                            &candidates,
                            drawdown_limit,
                            spec.search
                                .as_ref()
                                .and_then(|search| search.robustness_subwindows),
                            spec.search
                                .as_ref()
                                .and_then(|search| search.minimum_training_robustness_score_bps),
                            || cancellation.is_some_and(CancellationToken::is_cancelled),
                        )?;
                        let Some(report) = report else {
                            return Ok(None);
                        };
                        (Some(report), Vec::new())
                    }
                }
            }
            None => (None, Vec::new()),
        };
        (out_of_sample.0, baselines, out_of_sample.1)
    } else {
        (None, Vec::new(), Vec::new())
    };
    let cost_robustness_scenario_names = spec
        .cost_sensitivity
        .iter()
        .filter(|scenario| scenario.max_training_return_degradation_bps.is_some())
        .map(|scenario| scenario.name.as_str())
        .collect::<Vec<_>>();
    let cost_robustness_passed = (!cost_robustness_scenario_names.is_empty()).then(|| {
        let scenario_passes = |results: &[ExecutionCostSensitivityResult]| {
            cost_robustness_scenario_names.iter().all(|name| {
                results
                    .iter()
                    .find(|result| result.name.as_str() == *name)
                    .and_then(|result| result.cost_robustness_passed)
                    == Some(true)
            })
        };
        if spec
            .search
            .as_ref()
            .is_some_and(|search| search.fold_local_search)
        {
            !fold_searches.is_empty()
                && fold_searches
                    .iter()
                    .all(|fold| scenario_passes(&fold.cost_sensitivity))
        } else {
            scenario_passes(&cost_sensitivity)
        }
    });
    let neighborhood_cost_scenario_names = spec
        .cost_sensitivity
        .iter()
        .filter(|scenario| {
            scenario
                .max_neighborhood_training_return_degradation_bps
                .is_some()
        })
        .map(|scenario| scenario.name.as_str())
        .collect::<Vec<_>>();
    let neighborhood_cost_robustness_passed =
        (!neighborhood_cost_scenario_names.is_empty()).then(|| {
            let scenarios_pass = |results: &[ExecutionCostSensitivityResult]| {
                neighborhood_cost_scenario_names.iter().all(|name| {
                    results
                        .iter()
                        .find(|result| result.name.as_str() == *name)
                        .and_then(|result| result.neighborhood_cost_robustness.as_ref())
                        .is_some_and(|result| result.passed)
                })
            };
            if spec
                .search
                .as_ref()
                .is_some_and(|search| search.fold_local_search)
            {
                !fold_searches.is_empty()
                    && fold_searches
                        .iter()
                        .all(|fold| scenarios_pass(&fold.cost_sensitivity))
            } else {
                scenarios_pass(&cost_sensitivity)
            }
        });
    let search_report = spec.search.as_ref().map(|plan| ExperimentSearchReport {
        algorithm: plan.algorithm,
        objective: if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
            ExperimentSearchObjective::ParetoReturnDrawdown
        } else {
            ExperimentSearchObjective::Return
        },
        seed: plan.seed,
        budget: plan.budget,
        max_training_drawdown_bps: plan.max_training_drawdown_bps,
        early_pruning: plan.early_pruning.clone(),
        robustness_subwindows: plan.robustness_subwindows,
        minimum_training_robustness_score_bps: plan.minimum_training_robustness_score_bps,
        cost_robustness_passed,
        neighborhood_cost_robustness_passed,
        neighborhood: if plan.continuous_parameter_space.is_empty() {
            neighborhood_report_with_radius_and_budget(
                &spec.parameter_space,
                &combinations,
                &search_trace,
                plan.neighborhood_radius.unwrap_or(1),
                plan.max_training_drawdown_bps,
            )
        } else {
            continuous_neighborhood_report_with_radius_and_budget(
                &plan.continuous_parameter_space,
                &search_trace,
                plan.neighborhood_radius.unwrap_or(1),
                plan.max_training_drawdown_bps,
            )
        },
        trials: search_trace,
        fold_searches,
    });
    let result = RunExperimentResult {
        schema_version: RUN_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        spec_fingerprint,
        canonical_run_spec_json: Some(canonical_run_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_run_spec_fingerprint.clone()),
        input_data_fingerprint: Some(input_data_fingerprint),
        result_hash: None,
        run_manifest: None,
        run_record: None,
        artifact_locations: None,
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates: successful.len(),
        failed_candidates: candidates
            .iter()
            .filter(|candidate| candidate.outcome.is_none() && candidate.pruning.is_none())
            .count(),
        pruned_candidates: candidates
            .iter()
            .filter(|candidate| candidate.pruning.is_some())
            .count(),
        candidates,
        comparison,
        out_of_sample,
        baselines,
        search: search_report,
        cost_sensitivity,
        artifact_path: report_index_path.clone(),
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建实验产物目录", &error))?;
    write_spec_artifact(&spec_artifact_path, &persisted_input, "参数实验")?;
    verify_experiment_candidate_outcomes(&result, context)?;
    let persisted = persist_experiment_result(
        result,
        &canonical_run_spec,
        &root,
        &report_index_path,
        &spec_artifact_path,
    )?;
    verify_persisted_experiment_result(&persisted)?;
    Ok(Some(persisted))
}

fn persist_experiment_result(
    mut result: RunExperimentResult,
    canonical_spec: &CanonicalRunSpec,
    root: &Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunExperimentResult, AppError> {
    let result_hash = experiment_result_hash(&result)?;
    result.result_hash = Some(result_hash.clone());
    let manifest =
        build_experiment_run_manifest(&result.experiment_id, canonical_spec, &result_hash)?;
    let manifest_path = persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    // Keep artifact_path as the stable read/write index. The immutable report is exposed by the
    // RunRecord-backed artifact_locations map.
    result.artifact_path = report_index_path.to_owned();
    result.run_manifest = Some(manifest_path.to_string_lossy().into_owned());
    result.run_record = Some(record_path.to_string_lossy().into_owned());
    result.artifact_locations = Some(BTreeMap::from([
        (
            "experiment_report".into(),
            report_path.to_string_lossy().into_owned(),
        ),
        (
            "run_manifest".into(),
            result.run_manifest.clone().expect("manifest set above"),
        ),
        ("experiment_input".into(), spec_artifact_path.to_owned()),
    ]));

    let report_payload = result.to_json()?;
    let manifest_payload = manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验 RunManifest 编码失败: {error}"),
        )
    })?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取参数实验输入规格 sidecar", &error))?;
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| experiment_storage_error("实验输入数据指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| experiment_storage_error("实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-experiment-result-json".into(),
                schema_version: RUN_EXPERIMENT_SCHEMA_VERSION,
                byte_len: report_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "run_manifest".into(),
                artifact_id: qx_strategy::sha256_hex(manifest_bytes),
                kind: "run-manifest-json".into(),
                schema_version: 1,
                byte_len: manifest_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "experiment_input".into(),
                artifact_id: qx_strategy::sha256_hex(&spec_bytes),
                kind: "run-experiment-input-json".into(),
                schema_version: 1,
                byte_len: spec_bytes.len() as u64,
            },
        ],
        replay_verdict: ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let record_payload = record.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验 RunRecord V2 编码失败: {error}"),
        )
    })?;
    let readback = RunRecordV2::from_json(&record_payload).map_err(|error| {
        experiment_storage_error(format!("参数实验 RunRecord V2 无效: {error}"))
    })?;
    if readback != record {
        return Err(experiment_storage_error(
            "参数实验 RunRecord V2 规范化读回与写入对象不一致",
        ));
    }

    std::fs::create_dir_all(root)
        .map_err(|error| AppError::from_io("创建参数实验产物目录", &error))?;
    let record_publish = crate::artifact_store::sink::publish(
        &record_path,
        &record_payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    );
    map_experiment_publish_result(record_publish, "参数实验 RunRecord V2")?;
    let report_publish = crate::artifact_store::sink::publish(
        &report_path,
        &report_payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    );
    map_experiment_publish_result(report_publish, "参数实验报告")?;
    write_text(report_index_path, &report_payload)?;
    Ok(result)
}

pub(crate) fn build_experiment_run_manifest(
    experiment_id: &str,
    canonical_spec: &CanonicalRunSpec,
    result_hash: &str,
) -> Result<RunManifest, AppError> {
    let dataset = canonical_spec
        .datasets
        .first()
        .ok_or_else(|| experiment_storage_error("实验 CanonicalRunSpec 缺少数据身份"))?;
    let runtime_version = canonical_spec
        .execution_model
        .get("runtime_version")
        .and_then(Value::as_str)
        .ok_or_else(|| experiment_storage_error("实验执行模型缺少 runtime_version"))?;
    let global_seed = canonical_spec
        .random_seed
        .parse::<u64>()
        .map_err(|error| experiment_storage_error(format!("实验随机种子无效: {error}")))?;
    let model_bytes = serde_json::to_vec(&canonical_spec.execution_model)
        .map_err(|error| experiment_storage_error(format!("实验执行模型规范化失败: {error}")))?;
    let instrument_spec_version = canonical_spec
        .market_rules
        .get(&dataset.instrument)
        .cloned()
        .unwrap_or_else(|| "legacy-default-v1".into());
    let strategy_version = if canonical_spec.run_type == "depth_experiment" {
        "depth-experiment-strategy-set-v1".into()
    } else if canonical_spec.strategies.len() == 1 {
        canonical_spec.strategies[0].strategy_version.clone()
    } else {
        "experiment-strategy-set-v1".into()
    };
    let canonical_json = canonical_spec.canonical_json().map_err(|error| {
        experiment_storage_error(format!("实验 CanonicalRunSpec 规范化失败: {error}"))
    })?;
    let canonical_fingerprint = canonical_spec.fingerprint().map_err(|error| {
        experiment_storage_error(format!("实验 CanonicalRunSpec 指纹失败: {error}"))
    })?;
    let input_components = canonical_spec
        .datasets
        .iter()
        .map(|dataset| (dataset.role.clone(), dataset.content_fingerprint.clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(RunManifest {
        run_id: experiment_id.to_owned(),
        code_commit: canonical_spec.code_revision.clone(),
        config_hash: canonical_fingerprint.clone(),
        data_fingerprint: dataset.content_fingerprint.clone(),
        input_components,
        clock_start: canonical_spec.clock.start_ts_ms,
        clock_end: canonical_spec.clock.end_ts_ms,
        global_seed,
        determinism_mode: true,
        result_hash: result_hash.to_owned(),
        strategy_version,
        instrument_spec_version,
        model_fingerprint: qx_strategy::sha256_hex(&model_bytes),
        input_event_hash: dataset.content_fingerprint.clone(),
        output_event_hash: result_hash.to_owned(),
        runtime_version: runtime_version.to_owned(),
        canonical_run_spec_json: Some(canonical_json),
        canonical_run_spec_fingerprint: Some(canonical_fingerprint),
    })
}

fn verify_experiment_candidate_outcomes(
    result: &RunExperimentResult,
    context: &RunContext,
) -> Result<(), AppError> {
    let mut run_ids = BTreeSet::new();
    for candidate in &result.candidates {
        if let Some(outcome) = &candidate.outcome {
            verify_one_experiment_outcome(outcome, context, &mut run_ids)?;
        }
    }
    for scenario in &result.cost_sensitivity {
        verify_cost_sensitivity_outcomes(scenario, context, &mut run_ids)?;
    }
    if let Some(search) = &result.search {
        for fold in &search.fold_searches {
            for candidate in &fold.candidates {
                if let Some(outcome) = &candidate.outcome {
                    verify_one_experiment_outcome(outcome, context, &mut run_ids)?;
                }
            }
            for scenario in &fold.cost_sensitivity {
                verify_cost_sensitivity_outcomes(scenario, context, &mut run_ids)?;
            }
        }
    }
    Ok(())
}

fn verify_cost_sensitivity_outcomes(
    scenario: &ExecutionCostSensitivityResult,
    context: &RunContext,
    verified_run_ids: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    if let Some(outcome) = &scenario.outcome {
        verify_one_experiment_outcome(outcome, context, verified_run_ids)?;
    }
    if let Some(neighborhood) = &scenario.neighborhood_cost_robustness {
        for candidate in &neighborhood.candidates {
            if let Some(outcome) = &candidate.outcome {
                verify_one_experiment_outcome(outcome, context, verified_run_ids)?;
            }
        }
    }
    Ok(())
}

fn verify_one_experiment_outcome(
    outcome: &BacktestOutcome,
    context: &RunContext,
    verified_run_ids: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    if !verified_run_ids.insert(outcome.run_id.clone()) {
        return Ok(());
    }
    let verification = verify_run(outcome, context)?;
    if !verification.verified {
        return Err(experiment_storage_error(format!(
            "实验候选 {} 产物复核失败: {}",
            outcome.run_id,
            verification.mismatches.join("; ")
        )));
    }
    Ok(())
}

fn verify_persisted_experiment_result(result: &RunExperimentResult) -> Result<(), AppError> {
    let value = serde_json::json!({
        "schema_version": result.schema_version,
        "experiment_id": result.experiment_id,
        "spec_fingerprint": result.spec_fingerprint,
        "canonical_run_spec_json": result.canonical_run_spec_json,
        "canonical_run_spec_fingerprint": result.canonical_run_spec_fingerprint,
        "input_data_fingerprint": result.input_data_fingerprint,
        "result_hash": result.result_hash,
        "run_manifest": result.run_manifest,
        "run_record": result.run_record,
        "artifact_locations": result.artifact_locations,
        "artifact_path": result.artifact_path,
    });
    let reader = crate::artifact_store::FileArtifactReader;
    crate::artifact_store::verify_declared_run_record_with_reader(&value, &reader).map_err(
        |error| experiment_storage_error(format!("参数实验 RunRecord V2 复核失败: {error}")),
    )?;
    crate::artifact_store::verify_declared_run_manifest_with_reader(&value, &reader).map_err(
        |error| experiment_storage_error(format!("参数实验 RunManifest 复核失败: {error}")),
    )
}

fn map_experiment_publish_result(
    result: Result<(), crate::artifact_store::sink::PublishError>,
    label: &str,
) -> Result<(), AppError> {
    result.map_err(|error| match error {
        crate::artifact_store::sink::PublishError::Conflict(message) => AppError::new(
            AppErrorCategory::Conflict,
            format!("{label}冲突: {message}"),
        ),
        crate::artifact_store::sink::PublishError::Io(error) => {
            AppError::from_io(&format!("发布{label}"), &error)
        }
    })
}

fn experiment_storage_error(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}

// The explicit identity fields are all part of the checkpoint cache key.
#[allow(clippy::too_many_arguments)]
fn load_search_candidate_checkpoint(
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

fn existing_result(
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

fn build_experiment_canonical_run_spec(
    spec: &RunExperimentSpec,
    identity: Option<&StrategyArtifactIdentity>,
    spec_json: &str,
    frame: &BarFrame,
    data_fingerprint: &str,
    context: &RunContext,
) -> Result<CanonicalRunSpec, AppError> {
    let start_ts_ms =
        frame.ts.first().copied().ok_or_else(|| {
            experiment_identity_invalid("CanonicalRunSpec 缺少 BarFrame 开始时间")
        })?;
    let end_ts_ms =
        frame.ts.last().copied().ok_or_else(|| {
            experiment_identity_invalid("CanonicalRunSpec 缺少 BarFrame 结束时间")
        })?;

    let portable_spec_json = quote_unsafe_integer_tokens(spec_json);
    let mut behavior: Value = serde_json::from_str(&portable_spec_json).map_err(|error| {
        experiment_identity_invalid(format!("RunExperimentSpec 规范化 JSON 无效: {error}"))
    })?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| experiment_identity_invalid("RunExperimentSpec 规范化结果不是 JSON 对象"))?;
    fields.remove("experiment_id");
    let base = fields
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| experiment_identity_invalid("实验 CanonicalRunSpec 缺少 base"))?;
    for field in ["run_id", "bars_path", "output_dir"] {
        base.remove(field);
    }
    if let Some(dimensions) = fields
        .get_mut("parameter_space")
        .and_then(Value::as_array_mut)
    {
        for dimension in dimensions {
            if let Some(values) = dimension.get_mut("values").and_then(Value::as_array_mut) {
                for value in values {
                    canonicalize_integer(value);
                }
            }
        }
    }
    if let Some(ranges) = fields
        .get_mut("search")
        .and_then(|search| search.get_mut("continuous_parameter_space"))
        .and_then(Value::as_array_mut)
    {
        for range in ranges {
            for field in ["min", "max", "step"] {
                if let Some(value) = range.get_mut(field) {
                    canonicalize_integer(value);
                }
            }
        }
    }
    normalize_experiment_identity_values(&mut behavior, "behavior")?;

    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![spec.base.strategy.kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let mut strategies = Vec::with_capacity(kinds.len());
    for kind in kinds {
        let mut strategy = spec.base.strategy.clone();
        strategy.kind = kind;
        let mut parameters = serde_json::to_value(&strategy).map_err(|error| {
            experiment_identity_invalid(format!("实验策略参数转 JSON 失败: {error}"))
        })?;
        normalize_experiment_identity_values(&mut parameters, "strategies.parameters")?;
        let strategy_id = if spec.strategy_kinds.is_empty() {
            strategy.strategy_id.clone()
        } else {
            format!("{}#{}", strategy.strategy_id, kind.name())
        };
        strategies.push(CanonicalStrategyIdentity {
            strategy_id,
            strategy_version: identity
                .map(|identity| identity.strategy_version.clone())
                .unwrap_or_else(|| super::run_backtest::strategy_slot::strategy_version(&strategy)),
            source_sha256: identity.map(|identity| identity.source_sha256.clone()),
            parameters,
        });
    }

    let mut market_rules = std::collections::BTreeMap::new();
    if let Some(market_spec) = &spec.base.market_spec {
        let mut market_rules_value = serde_json::to_value(market_spec)
            .map_err(|error| experiment_identity_invalid(format!("市场规则转换失败: {error}")))?;
        normalize_experiment_identity_values(&mut market_rules_value, "market_rules")?;
        let bytes = serde_json::to_vec(&market_rules_value)
            .map_err(|error| experiment_identity_invalid(format!("市场规则编码失败: {error}")))?;
        market_rules.insert(
            spec.base.instrument.clone(),
            qx_strategy::sha256_hex(&bytes),
        );
    }

    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": crate::cases::kernels::BAR_MATCHING_KERNEL,
        "fill_model": match spec.base.fill_model.unwrap_or(crate::BacktestFillModel::NextBarOpen) {
            crate::BacktestFillModel::NextBarOpen => "next_bar_open",
            crate::BacktestFillModel::BestPrice => "best_price",
            crate::BacktestFillModel::OneTickSlippage => "one_tick_slippage",
        },
        "risk_rule_set_version": spec.base.risk_rules.as_ref().map(|rules| rules.version.as_str()).unwrap_or("runtime-risk-v1"),
        "account_id": spec.base.account_id.as_deref().unwrap_or("app"),
        "settlement_currency": spec.base.settlement_currency,
    });
    CanonicalRunSpec::new(
        "experiment",
        context.code_commit(),
        strategies,
        vec![CanonicalDatasetIdentity {
            role: "bars".into(),
            instrument: spec.base.instrument.clone(),
            content_fingerprint: data_fingerprint.into(),
            start_ts_ms,
            end_ts_ms,
        }],
        market_rules,
        CanonicalRunClock {
            start_ts_ms,
            end_ts_ms,
            timestamp_unit: "unix_ms".into(),
            timezone: "UTC".into(),
        },
        spec.base.seed,
        behavior,
        execution_model,
        std::collections::BTreeMap::new(),
    )
    .map_err(|error| experiment_identity_invalid(format!("实验 CanonicalRunSpec 无效: {error}")))
}

pub(crate) fn normalize_experiment_identity_values(
    value: &mut Value,
    path: &str,
) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(experiment_identity_invalid(format!(
            "{path} 含浮点值；CanonicalRunSpec 精确数值必须使用十进制文本"
        ))),
        Value::Number(number) => {
            if number
                .as_i64()
                .is_some_and(|integer| integer.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|integer| integer > 9_007_199_254_740_991)
            {
                canonicalize_integer(value);
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                normalize_experiment_identity_values(child, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, child) in fields.iter_mut() {
                if name.ends_with("_raw") || name == "seed" {
                    canonicalize_integer(child);
                }
                normalize_experiment_identity_values(child, &format!("{path}.{name}"))?;
            }
            Ok(())
        }
    }
}

fn canonicalize_integer(value: &mut Value) {
    if value.is_number() {
        *value = Value::String(value.to_string());
    }
}

pub(crate) fn quote_unsafe_integer_tokens(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut output = String::with_capacity(json.len());
    let mut segment_start = 0;
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            index += 1;
            continue;
        }
        if byte.is_ascii_digit() || byte == b'-' {
            let start = index;
            while index < bytes.len()
                && !matches!(
                    bytes[index],
                    b',' | b']' | b'}' | b':' | b' ' | b'\n' | b'\r' | b'\t'
                )
            {
                index += 1;
            }
            let token = &json[start..index];
            let is_unsafe_integer = !token.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
                && token
                    .parse::<i128>()
                    .map(|number| number.unsigned_abs() > 9_007_199_254_740_991)
                    .unwrap_or_else(|_| {
                        token
                            .parse::<u128>()
                            .is_ok_and(|number| number > 9_007_199_254_740_991)
                    });
            if is_unsafe_integer {
                output.push_str(&json[segment_start..start]);
                output.push('"');
                output.push_str(token);
                output.push('"');
                segment_start = index;
            }
            continue;
        }
        index += 1;
    }
    output.push_str(&json[segment_start..]);
    output
}

fn validate_experiment_run_identity(result: &RunExperimentResult) -> Result<(), AppError> {
    match (
        result.canonical_run_spec_json.as_deref(),
        result.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (None, None) => Ok(()),
        (Some(payload), Some(expected_fingerprint)) => {
            let canonical = CanonicalRunSpec::from_json(payload)
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let canonical_json = canonical
                .canonical_json()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let fingerprint = canonical
                .fingerprint()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            if canonical_json != payload
                || fingerprint != expected_fingerprint
                || canonical.run_type != "experiment"
                || canonical.datasets.len() != 1
                || result.input_data_fingerprint.as_deref()
                    != canonical
                        .datasets
                        .first()
                        .map(|dataset| dataset.content_fingerprint.as_str())
                || result
                    .candidates
                    .iter()
                    .filter_map(|candidate| candidate.outcome.as_ref())
                    .any(|outcome| canonical.datasets[0].instrument != outcome.instrument)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "参数实验 CanonicalRunSpec 与报告数据身份或指纹不一致",
                ));
            }
            Ok(())
        }
        _ => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "参数实验 CanonicalRunSpec JSON 与 fingerprint 必须同时存在或同时缺省",
        )),
    }
}

fn validate_experiment_artifact_envelope(result: &RunExperimentResult) -> Result<(), AppError> {
    match (
        result.result_hash.as_deref(),
        result.run_manifest.as_deref(),
        result.run_record.as_deref(),
        result.artifact_locations.as_ref(),
    ) {
        (None, None, None, None) => Ok(()),
        (Some(result_hash), Some(manifest), Some(record), Some(locations)) => {
            let is_hash = |value: &str| {
                value.len() == 16
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            };
            if !is_hash(result_hash)
                || manifest.trim().is_empty()
                || record.trim().is_empty()
                || result.canonical_run_spec_json.is_none()
                || result.canonical_run_spec_fingerprint.is_none()
                || result.input_data_fingerprint.is_none()
                || !matches!(locations.len(), 2 | 3)
                || (locations.len() == 3
                    && locations
                        .get("experiment_input")
                        .is_none_or(|path| path.trim().is_empty()))
                || locations
                    .get("experiment_report")
                    .is_none_or(|path| path.trim().is_empty())
                || locations.get("run_manifest").map(String::as_str) != Some(manifest)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "参数实验标准产物指针或 ArtifactLocations 无效",
                ));
            }
            if experiment_result_hash(result)? != result_hash {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "参数实验 result_hash 与报告事实不一致",
                ));
            }
            Ok(())
        }
        _ => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "参数实验 result_hash、RunManifest、RunRecord 和 ArtifactLocations 必须全部存在或全部缺省",
        )),
    }
}

fn experiment_result_hash(result: &RunExperimentResult) -> Result<String, AppError> {
    let serialized = serde_json::to_string(result).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验事实摘要编码失败: {error}"),
        )
    })?;
    experiment_result_hash_from_json(&serialized).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验事实摘要规范化失败: {error}"),
        )
    })
}

pub(crate) fn experiment_result_hash_from_value(value: &Value) -> Result<String, String> {
    let payload = serde_json::to_string(value)
        .map_err(|error| format!("参数实验事实摘要编码失败: {error}"))?;
    experiment_result_hash_from_json(&payload)
}

pub(crate) fn experiment_result_hash_from_json(payload: &str) -> Result<String, String> {
    let mut parser = ExperimentJsonParser::new(payload);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.cursor != parser.bytes.len() {
        return Err("参数实验 JSON 存在尾随内容".into());
    }
    let mut canonical = String::new();
    value.write_canonical(&mut canonical);
    let mut digest = Fnv1a::new();
    digest.write_bytes(canonical.as_bytes());
    Ok(format!("{:016x}", digest.finish()))
}

#[derive(Debug)]
enum ExperimentJsonValue {
    Raw(String),
    Array(Vec<ExperimentJsonValue>),
    Object(BTreeMap<String, ExperimentJsonValue>),
}

impl ExperimentJsonValue {
    fn write_canonical(&self, output: &mut String) {
        match self {
            Self::Raw(value) => output.push_str(value),
            Self::Array(items) => {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    item.write_canonical(output);
                }
                output.push(']');
            }
            Self::Object(fields) => {
                output.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    output.push_str(&serde_json::to_string(name).expect("string key serializes"));
                    output.push(':');
                    value.write_canonical(output);
                }
                output.push('}');
            }
        }
    }
}

struct ExperimentJsonParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ExperimentJsonParser<'a> {
    fn new(payload: &'a str) -> Self {
        Self {
            bytes: payload.as_bytes(),
            cursor: 0,
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.cursor += 1;
        }
    }

    fn parse_value(&mut self) -> Result<ExperimentJsonValue, String> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor).copied() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_raw_string(),
            Some(_) => self.parse_raw_scalar(),
            None => Err("参数实验 JSON 意外结束".into()),
        }
    }

    fn parse_object(&mut self) -> Result<ExperimentJsonValue, String> {
        self.cursor += 1;
        let mut fields = BTreeMap::new();
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(ExperimentJsonValue::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key_start = self.cursor;
            let key_end = self.scan_string()?;
            let key: String = serde_json::from_slice(&self.bytes[key_start..key_end])
                .map_err(|error| format!("参数实验 JSON 对象键无效: {error}"))?;
            self.skip_whitespace();
            if !self.consume(b':') {
                return Err("参数实验 JSON 对象缺少冒号".into());
            }
            let value = self.parse_value()?;
            if !is_experiment_location_key(&key) {
                fields.insert(key, value);
            }
            self.skip_whitespace();
            if self.consume(b'}') {
                break;
            }
            if !self.consume(b',') {
                return Err("参数实验 JSON 对象缺少逗号".into());
            }
        }
        Ok(ExperimentJsonValue::Object(fields))
    }

    fn parse_array(&mut self) -> Result<ExperimentJsonValue, String> {
        self.cursor += 1;
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.consume(b']') {
            return Ok(ExperimentJsonValue::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            if self.consume(b']') {
                break;
            }
            if !self.consume(b',') {
                return Err("参数实验 JSON 数组缺少逗号".into());
            }
        }
        Ok(ExperimentJsonValue::Array(values))
    }

    fn parse_raw_string(&mut self) -> Result<ExperimentJsonValue, String> {
        let start = self.cursor;
        let end = self.scan_string()?;
        let raw = std::str::from_utf8(&self.bytes[start..end])
            .map_err(|error| format!("参数实验 JSON 字符串不是 UTF-8: {error}"))?;
        Ok(ExperimentJsonValue::Raw(raw.to_owned()))
    }

    fn scan_string(&mut self) -> Result<usize, String> {
        if !self.consume(b'"') {
            return Err("参数实验 JSON 对象键必须是字符串".into());
        }
        let mut escaped = false;
        while let Some(byte) = self.bytes.get(self.cursor).copied() {
            self.cursor += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Ok(self.cursor);
            }
        }
        Err("参数实验 JSON 字符串没有结束".into())
    }

    fn parse_raw_scalar(&mut self) -> Result<ExperimentJsonValue, String> {
        let start = self.cursor;
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b',' | b']' | b'}'))
        {
            self.cursor += 1;
        }
        if start == self.cursor {
            return Err("参数实验 JSON 标量为空".into());
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.cursor])
            .map_err(|error| format!("参数实验 JSON 标量不是 UTF-8: {error}"))?;
        Ok(ExperimentJsonValue::Raw(raw.to_owned()))
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }
}

fn is_experiment_location_key(name: &str) -> bool {
    matches!(
        name,
        "experiment_id"
            | "spec_fingerprint"
            | "artifact_path"
            | "artifacts"
            | "run_id"
            | "run_manifest"
            | "run_record"
            | "artifact_locations"
            | "result_hash"
            | "selected_run_manifest"
    ) || name.ends_with("_path")
        || name.ends_with("_file")
        || name.ends_with("_dir")
}

fn experiment_identity_invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn has_retryable_search_failures(result: &RunExperimentResult) -> bool {
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
