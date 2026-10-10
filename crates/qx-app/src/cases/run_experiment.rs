//! Deterministic Cartesian parameter experiments over the shared Rust backtest use case.
//!
//! This module only expands and records strategy parameters. Every candidate is executed by
//! [`crate::cases::run_backtest::run_backtest`], and successful candidates are ranked by the
//! shared [`crate::cases::compare_runs::compare_runs`] use case; it contains no matcher,
//! risk, ledger, or metrics implementation.

use super::compare_runs::COMPARE_RUNS_SCHEMA_VERSION;
use super::{attach, compare_runs, run_backtest};
use crate::cases::guard::guard_panics;
use crate::{
    AppError, AppErrorCategory, BacktestOutcome, BacktestSpec, CallerCapability, CompareRunsResult,
    CompareRunsSpec, ComparedRun, RunContext,
};
use qx_core::Fnv1a;
use qx_strategy::builtin_signal::BuiltinSignalKnob;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const RUN_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_EXPERIMENT_CANDIDATES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentParameterSpace {
    /// One of `fast_window`, `slow_window`, `period`, `threshold_bps`, or `quantity_raw`.
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
        if self.experiment_id.is_empty()
            || self.experiment_id.len() > 96
            || matches!(self.experiment_id.as_str(), "." | "..")
            || !self
                .experiment_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "experiment_id 必须是最多 96 字符的 [A-Za-z0-9._-] 标识",
            ));
        }
        self.base.validate()?;
        if self.parameter_space.is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "参数实验至少需要一个参数维度",
            ));
        }
        let supported = [
            "fast_window",
            "slow_window",
            "period",
            "threshold_bps",
            "quantity_raw",
        ];
        let mut names = BTreeSet::new();
        let mut total = 1_usize;
        for dimension in &self.parameter_space {
            if !supported.contains(&dimension.name.as_str()) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("不支持的策略参数维度: {}", dimension.name),
                ));
            }
            if !names.insert(dimension.name.as_str()) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("实验参数维度重复: {}", dimension.name),
                ));
            }
            let applies = match dimension.name.as_str() {
                "fast_window" => self
                    .base
                    .strategy
                    .kind
                    .uses_signal_knob(BuiltinSignalKnob::FastWindow),
                "slow_window" => self
                    .base
                    .strategy
                    .kind
                    .uses_signal_knob(BuiltinSignalKnob::SlowWindow),
                "period" => self
                    .base
                    .strategy
                    .kind
                    .uses_signal_knob(BuiltinSignalKnob::Period),
                "threshold_bps" => self
                    .base
                    .strategy
                    .kind
                    .uses_signal_knob(BuiltinSignalKnob::ThresholdBps),
                "quantity_raw" => true,
                _ => false,
            };
            if !applies {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "参数维度 {} 不适用于策略 {}",
                        dimension.name,
                        self.base.strategy.kind.name()
                    ),
                ));
            }
            if dimension.values.is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("参数维度 {} 没有候选值", dimension.name),
                ));
            }
            let unique: BTreeSet<_> = dimension.values.iter().collect();
            if unique.len() != dimension.values.len() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("参数维度 {} 含重复候选值", dimension.name),
                ));
            }
            total = total
                .checked_mul(dimension.values.len())
                .ok_or_else(|| AppError::new(AppErrorCategory::InvalidInput, "实验候选数溢出"))?;
            if total > MAX_EXPERIMENT_CANDIDATES {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("实验最多接受 {MAX_EXPERIMENT_CANDIDATES} 个候选组合"),
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
    context.require(CallerCapability::Research, "参数回测实验")?;
    let correlation_id = context.correlation_id().to_string();
    let result = guard_panics(&correlation_id, || run_experiment_inner(spec, context));
    result.map_err(|error| attach(error, &correlation_id))
}

fn run_experiment_inner(
    spec: &RunExperimentSpec,
    context: &RunContext,
) -> Result<RunExperimentResult, AppError> {
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
        Ok(existing) => return existing_result(&existing, &spec_fingerprint, &spec.experiment_id),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有实验报告", &error)),
    }
    let mut dimensions = spec.parameter_space.clone();
    dimensions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut combinations = vec![BTreeMap::new()];
    for dimension in &dimensions {
        let mut expanded = Vec::with_capacity(combinations.len() * dimension.values.len());
        for combination in &combinations {
            for value in &dimension.values {
                let mut next = combination.clone();
                next.insert(dimension.name.clone(), *value);
                expanded.push(next);
            }
        }
        combinations = expanded;
    }
    debug_assert_eq!(combinations.len(), total);

    let mut candidates = Vec::with_capacity(total);
    for (index, parameters) in combinations.into_iter().enumerate() {
        let ordinal = index + 1;
        let mut candidate = spec.base.clone();
        candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
        candidate.output_dir = root
            .join(format!("run-{ordinal:04}"))
            .to_string_lossy()
            .into();
        let outcome = match apply_parameters(&mut candidate, &parameters) {
            Ok(()) => run_backtest(&candidate, context),
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
        artifact_path: artifact_path.clone(),
    };
    let report_json = result.to_json()?;
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建实验产物目录", &error))?;
    if let Err(write_error) = super::artifacts::write_text(&artifact_path, &report_json) {
        // Concurrent identical requests can finish their candidates together. The first report
        // wins; accept it only after reading and validating the exact experiment identity.
        if let Ok(existing) = std::fs::read_to_string(&artifact_path) {
            return existing_result(&existing, &result.spec_fingerprint, &result.experiment_id);
        }
        return Err(write_error);
    }
    Ok(result)
}

fn apply_parameters(
    candidate: &mut BacktestSpec,
    parameters: &BTreeMap<String, i128>,
) -> Result<(), AppError> {
    for (name, value) in parameters {
        match name.as_str() {
            "fast_window" => {
                candidate.strategy.fast_window =
                    usize::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
            }
            "slow_window" => {
                candidate.strategy.slow_window =
                    usize::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
            }
            "period" => {
                candidate.strategy.period =
                    usize::try_from(*value).map_err(|_| invalid_parameter(name, *value))?
            }
            "threshold_bps" => candidate.strategy.threshold_bps = *value,
            "quantity_raw" => candidate.strategy.quantity_raw = *value,
            _ => unreachable!("validate() closes parameter names"),
        }
    }
    Ok(())
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

fn invalid_parameter(name: &str, value: i128) -> AppError {
    AppError::new(
        AppErrorCategory::InvalidInput,
        format!("实验参数 {name}={value} 超出可表示范围"),
    )
}
