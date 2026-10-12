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

use super::artifacts::read_text;
use super::compare_runs::COMPARE_RUNS_SCHEMA_VERSION;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_experiment_id, validate_grid,
};
use super::experiment_split::{
    evaluate_baselines, evaluate_out_of_sample, ExperimentBaselineReport,
    ExperimentOutOfSampleReport, ExperimentSplitPlan,
};
use super::guard::guard_panics;
use super::run_backtest::run_backtest_inner;
use super::{attach, compare_runs};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, BacktestOutcome, BacktestSpec, CallerCapability, CompareRunsResult,
    CompareRunsSpec, ComparedRun, RunContext, RunHandle,
};
use qx_core::Fnv1a;
use qx_datastruct::BarFrame;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const RUN_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_EXPERIMENT_CANDIDATES: usize = 256;

/// 基准对照的取值域。与 `qx_spec::EXPERIMENT_BASELINES` **逐值相等**——两份声明并存是
/// 因为 `qx-app` 的依赖表是精确且被门禁逐项核过的，为三个字符串把 spec crate 拉进来会让
/// 依赖面变宽。相等这件事由 `experiment_split_plan_check` 钉住，不靠人记得同步。
pub const EXPERIMENT_BASELINE_NAMES: [&str; 3] = ["buy-and-hold", "zero-signal", "none"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentParameterSpace {
    /// One of `fast_window`, `slow_window`, `period`, `threshold_bps`, `quantity_raw`, or `target_kelly_fraction_bps`.
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
}

impl RunExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("RunExperimentSpec JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
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
        // Validate stable base fields without rejecting candidate-specific values.
        let mut base = self.base.clone();
        base.strategy = normalized_strategy(&base.strategy, &self.parameter_space);
        base.validate()?;
        let total = validate_grid(&self.base.strategy, &self.parameter_space)?;
        if let Some(plan) = &self.split_plan {
            plan.validate()?;
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
        Ok(total)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentCandidateResult {
    pub ordinal: usize,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<BacktestOutcome>,
    /// Stable AppError JSON for a failed candidate; a failure does not hide other candidates.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<ExperimentCandidateResult>,
    pub comparison: Option<CompareRunsResult>,
    /// 样本外评估。spec 没写切分计划时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_of_sample: Option<ExperimentOutOfSampleReport>,
    /// 基准对照列。spec 没请求基准时为空表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baselines: Vec<ExperimentBaselineReport>,
    pub artifact_path: String,
}

impl RunExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("RunExperimentResult JSON 无法读取: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
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
        run_experiment_inner(spec, context, None)
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
                run_experiment_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

fn run_experiment_inner(
    spec: &RunExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<RunExperimentResult>, AppError> {
    let total = spec.validate()?;
    let spec_json = spec.to_json()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec_json);
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let artifact_path = root
        .join(format!("{}.experiment.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            return existing_result(&existing, &spec_fingerprint, &spec.experiment_id).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有实验报告", &error)),
    }
    // Split bounds and resource limits are preflighted before any candidate is run. Keep the
    // validated BarFrame for the report instead of reading it a second time after the grid.
    let evaluation_bars = if spec.split_plan.is_some() || !spec.baselines.is_empty() {
        let payload = read_text(&spec.base.bars_path)?;
        let frame = BarFrame::from_json(&payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BarFrame 校验失败 {}: {error:?}", spec.base.bars_path),
            )
        })?;
        let bars: Vec<_> = (&frame).into();
        if let Some(plan) = &spec.split_plan {
            plan.folds(bars.len())?;
        }
        Some(bars)
    } else {
        None
    };
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
            .into();
        let outcome = match apply_parameters(&mut candidate.strategy, &parameters) {
            Ok(()) => match run_backtest_inner(&candidate, context, cancellation) {
                Ok(Some(outcome)) => Ok(outcome),
                Ok(None) => return Ok(None),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        candidates.push(match outcome {
            Ok(outcome) => ExperimentCandidateResult {
                ordinal,
                parameters,
                outcome: Some(outcome),
                error: None,
            },
            Err(error) => ExperimentCandidateResult {
                ordinal,
                parameters,
                outcome: None,
                error: Some(error.to_json()),
            },
        });
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
    let (out_of_sample, baselines) = if let Some(bars) = evaluation_bars.as_deref() {
        let baselines = evaluate_baselines(bars, &spec.baselines);
        let out_of_sample = match &spec.split_plan {
            Some(plan) => {
                let report = evaluate_out_of_sample(plan, bars.len(), &candidates, || {
                    cancellation.is_some_and(CancellationToken::is_cancelled)
                })?;
                let Some(report) = report else {
                    return Ok(None);
                };
                Some(report)
            }
            None => None,
        };
        (out_of_sample, baselines)
    } else {
        (None, Vec::new())
    };
    let result = RunExperimentResult {
        schema_version: RUN_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        spec_fingerprint,
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates: successful.len(),
        failed_candidates: total - successful.len(),
        candidates,
        comparison,
        out_of_sample,
        baselines,
        artifact_path: artifact_path.clone(),
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    let report_json = result.to_json()?;
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建实验产物目录", &error))?;
    if let Err(write_error) = super::artifacts::write_text(&artifact_path, &report_json) {
        // Concurrent identical requests can finish their candidates together. The first report
        // wins; accept it only after reading and validating the exact experiment identity.
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
) -> Result<RunExperimentResult, AppError> {
    let previous = RunExperimentResult::from_json(payload)?;
    if previous.experiment_id != experiment_id || previous.spec_fingerprint != expected_fingerprint
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同实验规格"),
        ));
    }
    Ok(previous)
}
